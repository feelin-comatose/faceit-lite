// Встраивается в faceit.com (только главный фрейм).
// Делает 3 вещи: применяет твою тему/CSS, ловит «матч найден», замечает комнату матча.
// Никаких автокликов — только уведомления.
(() => {
  if (window.__FL_LOADED__) return;
  window.__FL_LOADED__ = true;

  const invoke = (cmd, args) => {
    try {
      return window.__TAURI_INTERNALS__?.invoke(cmd, args) ?? Promise.resolve(null);
    } catch {
      return Promise.resolve(null);
    }
  };

  let state = window.__FL_INIT__ || { config: {}, css: "" };
  let matchRe = /^(accept|принять)$/i;
  let lastMatchAlert = 0;

  // ───────── тема ─────────
  function buildCss({ config: c = {}, css = "" }) {
    const t = c.theme || {};
    let out = `:root{--fl-accent:${t.accent};--fl-bg:${t.bg};--fl-panel:${t.panel};--fl-text:${t.text};--fl-muted:${t.muted};--fl-good:${t.good};--fl-bad:${t.bad};--fl-radius:${t.radius}px}\n`;
    if (t.page_bg) out += `html,body,#root,#__next{background:${t.page_bg}!important}\n`;
    if (t.page_font) out += `body,button,input,textarea,select{font-family:${t.page_font}!important}\n`;
    if (c.low_power) {
      out +=
        "*,*::before,*::after{animation-duration:0s!important;animation-delay:0s!important;" +
        "transition-duration:0s!important;transition-delay:0s!important;scroll-behavior:auto!important}\n" +
        "video{display:none!important}\n";
    }
    const hide = (c.hide_selectors || []).filter((s) => s && s.trim());
    if (hide.length) out += `${hide.join(",")}{display:none!important}\n`;
    return out + "\n/* user faceit.css */\n" + css;
  }

  function styleEl() {
    let el = document.getElementById("fl-style");
    if (!el) {
      el = document.createElement("style");
      el.id = "fl-style";
    }
    return el;
  }

  // Держим наш <style> последним в <head>, чтобы перебивать стили сайта
  function keepLast() {
    const head = document.head;
    if (!head) return;
    const el = styleEl();
    if (head.lastElementChild !== el) head.appendChild(el);
  }

  function apply(next) {
    state = next || state;
    try {
      matchRe = new RegExp(state.config.match_found_regex || "^(accept|принять)$", "i");
    } catch {
      matchRe = /^(accept|принять)$/i;
    }
    styleEl().textContent = buildCss(state);
    keepLast();
  }
  window.__FL_apply = apply;

  // ───────── матч найден ─────────
  function beep() {
    if (!state.config.sound_on_match) return;
    try {
      const ctx = new AudioContext();
      [0, 0.18, 0.36].forEach((t, i) => {
        const o = ctx.createOscillator();
        const g = ctx.createGain();
        o.frequency.value = i === 2 ? 1320 : 880;
        g.gain.setValueAtTime(0.15, ctx.currentTime + t);
        g.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + t + 0.15);
        o.connect(g).connect(ctx.destination);
        o.start(ctx.currentTime + t);
        o.stop(ctx.currentTime + t + 0.16);
      });
      setTimeout(() => ctx.close(), 1000);
    } catch {}
  }

  function matchFound() {
    const now = Date.now();
    if (now - lastMatchAlert < 30000) return; // не спамим
    lastMatchAlert = now;
    beep();
    invoke("page_event", { kind: "match_found" });
  }

  function isAcceptButton(el) {
    if (!el || el.nodeType !== 1) return false;
    const btn = el.closest?.("button,[role=button]");
    if (!btn) return false;
    const text = (btn.textContent || "").trim();
    return text.length < 20 && matchRe.test(text) && btn.getClientRects().length > 0;
  }

  // Смотрим только добавленные/изменённые узлы — дёшево даже на тяжёлом SPA.
  // MutationObserver не троттлится в фоне, в отличие от setInterval.
  function scanNode(node) {
    if (node.nodeType === 3) return isAcceptButton(node.parentElement);
    if (node.nodeType !== 1) return false;
    if (isAcceptButton(node)) return true;
    const list = node.querySelectorAll?.("button,[role=button]");
    if (!list) return false;
    for (let i = 0; i < list.length && i < 200; i++) if (isAcceptButton(list[i])) return true;
    return false;
  }

  const observer = new MutationObserver((muts) => {
    for (const m of muts) {
      if (m.type === "characterData") {
        if (scanNode(m.target)) return matchFound();
        continue;
      }
      for (const n of m.addedNodes) if (scanNode(n)) return matchFound();
    }
  });

  // Заголовок вкладки тоже бывает сигналом
  const titleRe = /match (found|ready)|матч найден/i;
  const titleObserver = new MutationObserver(() => {
    if (titleRe.test(document.title)) matchFound();
  });

  // Head: следим, чтобы наш стиль оставался последним
  const headObserver = new MutationObserver(keepLast);

  // ───────── комната матча ─────────
  let lastRoom = null;
  function checkRoom() {
    const m = location.pathname.match(/\/room\/(1-[0-9a-f-]{36})/i);
    const id = m ? m[1] : null;
    if (id && id !== lastRoom) invoke("page_event", { kind: "room", data: id });
    lastRoom = id;
  }
  for (const fn of ["pushState", "replaceState"]) {
    const orig = history[fn];
    history[fn] = function (...a) {
      const r = orig.apply(this, a);
      queueMicrotask(checkRoom);
      return r;
    };
  }
  addEventListener("popstate", checkRoom);

  // ───────── старт ─────────
  function start() {
    apply();
    headObserver.observe(document.head, { childList: true });
    observer.observe(document.body, { childList: true, subtree: true, characterData: true });
    const title = document.querySelector("title");
    if (title) titleObserver.observe(title, { childList: true, characterData: true, subtree: true });
    checkRoom();
    // Берём актуальный конфиг (вдруг тему меняли без перезапуска)
    invoke("page_event", { kind: "ready" }).then((p) => p && apply(p));
  }

  if (document.readyState === "loading") addEventListener("DOMContentLoaded", start, { once: true });
  else start();
})();
