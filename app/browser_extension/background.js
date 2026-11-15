// Tapswitch — вкладки браузера для кнопок панели.
//
// Расширение подключается к приложению по WebSocket на этом компьютере (127.0.0.1),
// присылает список вкладок при каждом изменении и по просьбе приложения делает вкладку
// активной. Окно браузера на передний план выводит само приложение.
// Работает в любом браузере на Chromium 116+: Chrome, Opera, Comet, Edge, Brave и т.п.

const PORT = 3001; // app/config.yaml → browser.port
const URL = `ws://127.0.0.1:${PORT}/tabs`;
const PING_MS = 20000; // сообщения по WebSocket не дают браузеру усыпить расширение
const RETRY_MS = 3000;
const SEND_DELAY_MS = 150; // несколько событий подряд — один список

// Бейдж на иконке: зелёная галочка — приложение подключено, красный крест — нет.
const BADGE_ON = "✓";
const BADGE_OFF = "✕";
const BADGE_ON_COLOR = "#22c55e";
const BADGE_OFF_COLOR = "#ef4444";

function setStatus(connected) {
  chrome.action.setBadgeText({ text: connected ? BADGE_ON : BADGE_OFF });
  chrome.action.setBadgeBackgroundColor({ color: connected ? BADGE_ON_COLOR : BADGE_OFF_COLOR });
  if (chrome.action.setBadgeTextColor) chrome.action.setBadgeTextColor({ color: "#ffffff" });
  chrome.action.setTitle({
    title: connected
      ? "Tapswitch — подключено к приложению"
      : "Tapswitch — нет подключения к приложению",
  });
}

let socket = null;
let connecting = false; // подключение уже идёт: второе не начинаем
let retryTimer = null;
let sendTimer = null;

async function instanceId() {
  // Своё у каждого браузера и профиля: так приложение различает Chrome и Opera.
  const stored = await chrome.storage.local.get("instance");
  if (stored.instance) return stored.instance;
  const id = crypto.randomUUID();
  await chrome.storage.local.set({ instance: id });
  return id;
}

function browserName() {
  if (navigator.userAgent.includes(" OPR/")) return "Opera";
  const brands = (navigator.userAgentData && navigator.userAgentData.brands) || [];
  const own = brands.map((b) => b.brand).find((b) => !/chromium|not.?a.?brand/i.test(b));
  return own || "Chromium";
}

async function snapshot() {
  const windows = await chrome.windows.getAll({ populate: true, windowTypes: ["normal"] });
  return windows
    .filter((w) => !w.incognito) // приватные окна приложению не показываются
    .map((w) => ({
      id: w.id,
      focused: w.focused,
      tabs: (w.tabs || []).map((t) => ({
        id: t.id,
        title: t.title || "",
        url: t.url || t.pendingUrl || "",
        active: t.active,
      })),
    }));
}

function send(message) {
  if (socket && socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(message));
}

function scheduleTabs() {
  clearTimeout(sendTimer);
  sendTimer = setTimeout(async () => send({ type: "tabs", windows: await snapshot() }), SEND_DELAY_MS);
}

async function activate(message) {
  try {
    await chrome.tabs.update(message.tab, { active: true });
    if (message.focus) {
      const win = await chrome.windows.get(message.window);
      await chrome.windows.update(message.window, {
        focused: true,
        ...(win.state === "minimized" ? { state: "normal" } : {}),
      });
    }
    send({ type: "activated", request: message.request, ok: true });
  } catch (error) {
    send({ type: "activated", request: message.request, ok: false, error: String(error) });
  }
  scheduleTabs();
}

async function connect() {
  if (connecting) return; // подключение уже идёт
  if (socket && socket.readyState <= WebSocket.OPEN) return; // уже подключено или подключается
  connecting = true;
  setStatus(false); // пока соединения нет
  try {
    await openSocket();
  } catch {
    retryLater();
  } finally {
    connecting = false;
  }
}

function retryLater() {
  clearTimeout(retryTimer);
  retryTimer = setTimeout(connect, RETRY_MS); // приложение ещё не запущено или перезапускается
}

async function openSocket() {
  clearTimeout(retryTimer);
  retryTimer = null;
  const id = await instanceId();
  // Пока ждали хранилище, соединение могло открыться из другого вызова.
  if (socket && socket.readyState <= WebSocket.OPEN) return;
  let ws;
  try {
    ws = new WebSocket(URL);
  } catch (error) {
    retryLater();
    return;
  }
  socket = ws;
  let ping = null;
  ws.onopen = async () => {
    setStatus(true);
    send({ type: "hello", instance: id, browser: browserName(), version: chrome.runtime.getManifest().version });
    send({ type: "tabs", windows: await snapshot() });
    ping = setInterval(() => send({ type: "ping" }), PING_MS);
  };
  ws.onmessage = (event) => {
    let message;
    try {
      message = JSON.parse(event.data);
    } catch {
      return;
    }
    if (message.type === "activate") activate(message);
    else if (message.type === "refresh") scheduleTabs();
  };
  ws.onclose = () => {
    clearInterval(ping);
    // Закрылось прежнее соединение, а текущее работает: значок не трогаем
    // и повторное подключение не запускаем.
    if (socket !== ws) return;
    socket = null;
    setStatus(false);
    retryLater();
  };
}

for (const event of [
  chrome.tabs.onActivated,
  chrome.tabs.onCreated,
  chrome.tabs.onRemoved,
  chrome.tabs.onMoved,
  chrome.tabs.onAttached,
  chrome.tabs.onDetached,
  chrome.tabs.onReplaced,
  chrome.windows.onCreated,
  chrome.windows.onRemoved,
  chrome.windows.onFocusChanged,
]) {
  event.addListener(() => {
    connect();
    scheduleTabs();
  });
}
chrome.tabs.onUpdated.addListener((tabId, change) => {
  if ("title" in change || "url" in change) scheduleTabs();
});

// Если браузер всё-таки усыпил расширение (приложение долго не запущено) — будильник
// раз в 30 с будит его и пробует подключиться снова.
chrome.alarms.create("reconnect", { periodInMinutes: 0.5 });
chrome.alarms.onAlarm.addListener(connect);
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
setStatus(false);
connect();
