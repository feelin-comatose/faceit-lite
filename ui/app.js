"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const esc = (v) =>
  String(v ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const num = (v) => {
  const n = parseFloat(String(v ?? "").replace(",", "."));
  return Number.isFinite(n) ? n : null;
};
const fmt = (v, d = 2) => (v == null ? "—" : v.toFixed(d));
// API FACEIT иногда меняет названия полей — берём первое найденное
const pick = (o, ...keys) => {
  for (const k of keys) if (o && o[k] != null && o[k] !== "") return o[k];
  return null;
};

let CONFIG = {};
let VERSION = "?";
const KEY_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

// ───────── тема ─────────
function applyTheme(cfg, panelCss) {
  const t = cfg.theme || {};
  $("#theme-vars").textContent = `:root{
    --accent:${t.accent};--bg:${t.bg};--panel:${t.panel};--text:${t.text};
    --muted:${t.muted};--good:${t.good};--bad:${t.bad};--radius:${t.radius}px;
    --font:${t.font || "system-ui"};
  }`;
  document.documentElement.style.opacity = Math.min(1, Math.max(0.3, t.panel_opacity || 1));
  if (panelCss != null) $("#user-css").textContent = panelCss;
}

async function loadConfig() {
  const { config, panel_css, version } = await invoke("get_config");
  CONFIG = config;
  VERSION = version || "?";
  $("#app-version").textContent = `Версия ${VERSION}`;
  applyTheme(config, panel_css);
  fillSettings(config);
  if (!$("#nick").value && config.nickname) $("#nick").value = config.nickname;
  return config;
}

// ───────── вкладки ─────────
function showTab(name) {
  $$(".tabs button").forEach((b) => b.classList.toggle("active", b.dataset.tab === name));
  $$(".tab").forEach((t) => t.classList.toggle("active", t.id === `tab-${name}`));
}
$$(".tabs button").forEach((b) => b.addEventListener("click", () => showTab(b.dataset.tab)));

const loading = (el) => (el.innerHTML = '<div class="loader"></div>');
const error = (el, e) => {
  let hint = "";
  if (!CONFIG.api_key) hint = "Вставь API-ключ во вкладке «Настройки».";
  else if (!KEY_RE.test(CONFIG.api_key.trim()))
    hint = "Ключ не похож на ключ FACEIT (должен быть вида xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx). Проверь в «Настройках».";
  el.innerHTML = `<div class="err">${esc(e)}${hint ? `<br><br>${esc(hint)}` : ""}<div class="small muted" style="margin-top:6px">FACEIT Lite ${esc(VERSION)}</div></div>`;
};

// ───────── уровни ─────────
const LEVELS = [0, 100, 501, 751, 901, 1051, 1201, 1351, 1531, 1751, 2001];
const lvlBadge = (lvl, sm) => `<span class="lvl lvl-${esc(lvl || 1)}${sm ? " sm" : ""}">${esc(lvl || "?")}</span>`;
function levelProgress(elo, lvl) {
  if (!elo || !lvl || lvl >= 10) return null;
  const from = LEVELS[lvl], to = LEVELS[lvl + 1];
  return { pct: Math.max(0, Math.min(100, ((elo - from) / (to - from)) * 100)), need: to - elo };
}

// ───────── профиль ─────────
$("#profile-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const nick = $("#nick").value.trim();
  if (nick) loadProfile(nick);
});

async function loadProfile(nick) {
  const out = $("#profile-out");
  loading(out);
  try {
    renderProfile(out, await invoke("get_player", { nickname: nick }));
  } catch (e) {
    error(out, e);
  }
}

function renderProfile(out, { player, stats, recent }) {
  const cs = player.games?.cs2 || {};
  const elo = cs.faceit_elo, lvl = cs.skill_level;
  const prog = levelProgress(elo, lvl);
  const life = stats?.lifetime || {};

  // последние матчи
  const rows = (recent?.items || []).map((it) => {
    const s = it.stats || {};
    const kills = num(pick(s, "Kills")), deaths = num(pick(s, "Deaths"));
    return {
      id: pick(s, "Match Id"),
      map: String(pick(s, "Map") || "?").replace(/^de_/, ""),
      win: String(pick(s, "Result")) === "1",
      score: pick(s, "Score") || "",
      kills, deaths,
      kd: num(pick(s, "K/D Ratio")) ?? (kills != null && deaths ? kills / deaths : null),
      adr: num(pick(s, "ADR")),
      hs: num(pick(s, "Headshots %")),
      at: num(pick(s, "Match Finished At", "Created At")),
    };
  });
  const avg = (arr) => (arr.length ? arr.reduce((a, b) => a + b, 0) / arr.length : null);
  const last = rows.slice(0, 20);
  const r = {
    wr: last.length ? (last.filter((x) => x.win).length / last.length) * 100 : null,
    kd: avg(last.map((x) => x.kd).filter((x) => x != null)),
    adr: avg(last.map((x) => x.adr).filter((x) => x != null)),
    hs: avg(last.map((x) => x.hs).filter((x) => x != null)),
  };

  // карты
  const maps = (stats?.segments || [])
    .filter((s) => s.type === "Map" && (!s.mode || s.mode === "5v5"))
    .map((s) => ({
      name: s.label,
      m: num(pick(s.stats, "Matches")) || 0,
      wr: num(pick(s.stats, "Win Rate %")),
      kd: num(pick(s.stats, "Average K/D Ratio", "K/D Ratio")),
    }))
    .sort((a, b) => b.m - a.m);

  const recentResults = (life["Recent Results"] || []).map((x) =>
    String(x) === "1" ? '<i class="w">W</i>' : '<i class="l">L</i>'
  );

  out.innerHTML = `
    <div class="card">
      <div class="player">
        <img src="${esc(player.avatar || "")}" alt="" onerror="this.style.visibility='hidden'">
        <div style="flex:1;min-width:0">
          <div class="name">${esc(player.nickname)}</div>
          <div class="muted small">${esc((player.country || "").toUpperCase())} · ${esc(cs.region || "")}</div>
        </div>
        <div style="text-align:right">
          <div style="display:flex;gap:8px;align-items:center;justify-content:flex-end">
            ${lvlBadge(lvl)}<span class="elo">${esc(elo ?? "—")}</span>
          </div>
          ${prog ? `<div class="muted small">+${prog.need} до ${lvl + 1} лвл</div>` : ""}
        </div>
      </div>
      ${prog ? `<div class="progress"><i style="width:${prog.pct}%"></i></div>` : ""}
      ${recentResults.length ? `<div class="streak">${recentResults.join("")}</div>` : ""}
    </div>

    <div class="card">
      <h4>Последние ${last.length} матчей</h4>
      <div class="kpis">
        <div class="kpi"><b class="${r.wr >= 50 ? "good" : "bad"}">${fmt(r.wr, 0)}%</b><span>Winrate</span></div>
        <div class="kpi"><b class="${r.kd >= 1 ? "good" : "bad"}">${fmt(r.kd)}</b><span>K/D</span></div>
        <div class="kpi"><b>${fmt(r.adr, 1)}</b><span>ADR</span></div>
        <div class="kpi"><b>${fmt(r.hs, 0)}%</b><span>HS</span></div>
      </div>
    </div>

    <div class="card">
      <h4>За всё время</h4>
      <div class="kpis">
        <div class="kpi"><b>${esc(pick(life, "Matches") ?? "—")}</b><span>Матчей</span></div>
        <div class="kpi"><b>${esc(pick(life, "Win Rate %") ?? "—")}%</b><span>Winrate</span></div>
        <div class="kpi"><b>${esc(pick(life, "Average K/D Ratio", "K/D Ratio") ?? "—")}</b><span>K/D</span></div>
        <div class="kpi"><b>${esc(pick(life, "Longest Win Streak") ?? "—")}</b><span>Макс. серия</span></div>
      </div>
    </div>

    ${rows.length ? `<div class="card"><h4>Матчи</h4><table>
      <tr><th>Карта</th><th>Счёт</th><th class="num">K-D</th><th class="num">K/D</th><th class="num">ADR</th><th class="num">HS</th></tr>
      ${rows.map((x) => `<tr class="match-row ${x.win ? "win" : "loss"}">
        <td>${esc(x.map)}</td><td class="muted">${esc(x.score)}</td>
        <td class="num">${x.kills ?? "—"}-${x.deaths ?? "—"}</td>
        <td class="num ${x.kd >= 1 ? "good" : "bad"}">${fmt(x.kd)}</td>
        <td class="num">${fmt(x.adr, 0)}</td><td class="num">${fmt(x.hs, 0)}%</td></tr>`).join("")}
    </table></div>` : ""}

    ${maps.length ? `<div class="card"><h4>Карты</h4><table>
      <tr><th>Карта</th><th class="num">Матчей</th><th class="num">WR</th><th class="num">K/D</th></tr>
      ${maps.map((m) => `<tr class="map-row"><td>${esc(m.name)}</td><td class="num">${m.m}</td>
        <td class="num ${m.wr >= 50 ? "good" : "bad"}">${fmt(m.wr, 0)}%</td><td class="num">${fmt(m.kd)}</td></tr>`).join("")}
    </table></div>` : ""}
  `;
}

// ───────── лобби ─────────
function parseMatchId(s) {
  const m = String(s).match(/1-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i);
  return m ? m[0] : String(s).trim();
}

$("#lobby-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const id = parseMatchId($("#match-id").value);
  if (id) loadLobby(id);
});

async function loadLobby(id) {
  $("#match-id").value = id;
  const out = $("#lobby-out");
  loading(out);
  try {
    renderLobby(out, await invoke("get_match", { matchId: id }));
  } catch (e) {
    error(out, e);
  }
}

function playerMaps(stats) {
  const res = {};
  for (const s of stats?.segments || []) {
    if (s.type !== "Map" || (s.mode && s.mode !== "5v5")) continue;
    const m = num(pick(s.stats, "Matches")) || 0;
    const wr = num(pick(s.stats, "Win Rate %")) || 0;
    const wins = num(pick(s.stats, "Wins")) ?? Math.round((m * wr) / 100);
    res[s.label] = { m, wins };
  }
  return res;
}

function renderLobby(out, { match, players }) {
  const me = (CONFIG.nickname || "").toLowerCase();
  const factions = ["faction1", "faction2"].map((f) => {
    const t = match.teams?.[f] || {};
    const roster = (t.roster || []).map((p) => {
      const full = players[p.player_id] || {};
      const cs = full.player?.games?.cs2 || {};
      const life = full.stats?.lifetime || {};
      return {
        nick: p.nickname,
        lvl: cs.skill_level || p.game_skill_level,
        elo: cs.faceit_elo ?? null,
        kd: num(pick(life, "Average K/D Ratio", "K/D Ratio")),
        wr: num(pick(life, "Win Rate %")),
        matches: num(pick(life, "Matches")),
        maps: playerMaps(full.stats),
        me: p.nickname?.toLowerCase() === me,
      };
    });
    const elos = roster.map((p) => p.elo).filter((x) => x != null);
    return {
      name: t.name || f,
      roster,
      avgElo: elos.length ? Math.round(elos.reduce((a, b) => a + b, 0) / elos.length) : null,
      mine: roster.some((p) => p.me),
    };
  });

  // «мы» — команда с моим ником, иначе первая
  if (factions[1].mine && !factions[0].mine) factions.reverse();
  const [us, them] = factions;

  // пул карт: из голосования, если есть, иначе все карты из статистики
  const pool = new Set(
    (match.voting?.map?.entities || []).map((e) => e.name || e.class_name || e.game_map_id).filter(Boolean)
  );
  const allMaps = new Set();
  for (const t of factions) for (const p of t.roster) Object.keys(p.maps).forEach((m) => allMaps.add(m));
  const mapNames = [...(pool.size ? pool : allMaps)];

  const teamOnMap = (team, map) => {
    let m = 0, w = 0;
    for (const p of team.roster) {
      const s = p.maps[map];
      if (s) { m += s.m; w += s.wins; }
    }
    return { m, wr: m ? (w / m) * 100 : null };
  };
  const mapRows = mapNames
    .map((name) => {
      const a = teamOnMap(us, name), b = teamOnMap(them, name);
      // приглушаем разницу, если матчей мало
      const conf = Math.min(1, Math.min(a.m, b.m) / 50);
      const diff = a.wr != null && b.wr != null ? (a.wr - b.wr) * conf : null;
      return { name, a, b, diff };
    })
    .sort((x, y) => (y.diff ?? -999) - (x.diff ?? -999));
  const scored = mapRows.filter((r) => r.diff != null);
  const picks = new Set(scored.filter((r) => r.diff > 0).slice(0, 2).map((r) => r.name));
  const bans = new Set(scored.filter((r) => r.diff < 0).slice(-2).map((r) => r.name));

  const picked = match.voting?.map?.pick?.[0];
  const score = match.results?.score;

  const teamHtml = (t) => `
    <div class="card team ${t.mine ? "mine" : ""}">
      <h4><span>${esc(t.name)}</span><span>${t.avgElo ?? "—"}</span></h4>
      ${t.roster.map((p) => `
        <div class="pl">${lvlBadge(p.lvl, true)}
          <div style="flex:1;min-width:0">
            <div class="n ${p.me ? "me" : ""}">${esc(p.nick)}</div>
            <div class="s">${p.elo ?? "—"} elo · K/D ${fmt(p.kd)} · ${fmt(p.wr, 0)}% · ${p.matches ?? "—"} м.</div>
          </div>
        </div>`).join("")}
    </div>`;

  out.innerHTML = `
    <div class="card">
      <div class="row" style="margin:0;justify-content:space-between">
        <span class="muted">${esc(match.status || "")}${picked ? ` · карта: <b>${esc(picked)}</b>` : ""}</span>
        <span>${score ? `${esc(score.faction1)} : ${esc(score.faction2)}` : ""}</span>
      </div>
      ${us.avgElo && them.avgElo ? `<div class="small muted">Разница среднего ELO: <b class="${us.avgElo >= them.avgElo ? "good" : "bad"}">${us.avgElo - them.avgElo > 0 ? "+" : ""}${us.avgElo - them.avgElo}</b></div>` : ""}
    </div>
    <div class="teams">${teamHtml(us)}${teamHtml(them)}</div>
    ${mapRows.length ? `<div class="card"><h4>Карты: ${us.mine ? "мы" : esc(us.name)} vs ${esc(them.name)}</h4><table>
      <tr><th>Карта</th><th class="num">Наш WR</th><th class="num">Их WR</th><th></th></tr>
      ${mapRows.map((r) => {
        const tag = picks.has(r.name) ? "pick" : bans.has(r.name) ? "ban" : "";
        return `<tr class="map-row ${tag}"><td>${esc(r.name)}</td>
          <td class="num">${fmt(r.a.wr, 0)}% <span class="muted small">${r.a.m}</span></td>
          <td class="num">${fmt(r.b.wr, 0)}% <span class="muted small">${r.b.m}</span></td>
          <td class="num">${tag ? `<span class="tag ${tag}">${tag === "pick" ? "ПИК" : "БАН"}</span>` : ""}</td></tr>`;
      }).join("")}
    </table><p class="small muted">WR суммарно по игрокам команды. Это подсказка по статам, а не гарантия.</p></div>` : ""}
  `;
}

// ───────── настройки ─────────
const getPath = (o, p) => p.split(".").reduce((a, k) => a?.[k], o);
const setPath = (o, p, v) => {
  const ks = p.split(".");
  const last = ks.pop();
  const t = ks.reduce((a, k) => (a[k] ??= {}), o);
  t[last] = v;
};

function fillSettings(cfg) {
  for (const el of $("#settings-form").elements) {
    if (!el.name) continue;
    const v = getPath(cfg, el.name);
    if (el.type === "checkbox") el.checked = !!v;
    else if (el.name === "hide_selectors") el.value = (v || []).join("\n");
    else el.value = v ?? "";
  }
}

$("#settings-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const cfg = structuredClone(CONFIG);
  for (const el of e.target.elements) {
    if (!el.name) continue;
    let v;
    if (el.type === "checkbox") v = el.checked;
    else if (el.type === "number") v = Number(el.value);
    else if (el.name === "hide_selectors") v = el.value.split("\n").map((s) => s.trim()).filter(Boolean);
    else v = el.value;
    setPath(cfg, el.name, v);
  }
  const msg = $("#settings-msg");
  cfg.api_key = (cfg.api_key || "").trim();
  try {
    await invoke("save_config", { config: cfg });
    msg.textContent = cfg.api_key && !KEY_RE.test(cfg.api_key)
      ? "Сохранено, но ключ не похож на ключ FACEIT (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx)"
      : "Сохранено ✓";
  } catch (err) {
    msg.textContent = "Ошибка: " + err;
  }
  setTimeout(() => (msg.textContent = ""), 6000);
});

$("#open-dir").addEventListener("click", () => invoke("open_config_dir").catch(alert));
$("#reload").addEventListener("click", () => invoke("reload_theme"));

// ───────── статус сайта ─────────
const time = () => new Date().toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });
let siteVisible = false;
let lastEvent = null; // { cls, text } — последний значимый статус (матч / комната)

function renderStatus() {
  const st = $("#status");
  const ev = lastEvent || (siteVisible
    ? { cls: "open", text: "FACEIT открыт" }
    : { cls: "", text: "FACEIT в трее · спит" });
  st.className = "status " + ev.cls;
  $("span", st).textContent = ev.text;
  const btn = $("#toggle-site");
  btn.textContent = siteVisible ? "Спрятать FACEIT" : "Открыть FACEIT";
  btn.classList.toggle("on", siteVisible);
}

$("#toggle-site").addEventListener("click", async () => {
  siteVisible = await invoke("toggle_site");
  renderStatus();
});

listen("site-status", ({ payload: p }) => {
  if (p.kind === "visibility") siteVisible = p.visible;
  else if (p.kind === "match_found") lastEvent = { cls: "found", text: `МАТЧ НАЙДЕН · ${time()} · жми Accept` };
  else if (p.kind === "room") lastEvent = { cls: "room", text: `В комнате матча · ${time()}` };
  renderStatus();
});

// «матч найден» гаснет через 2 минуты, комната — через 90 минут
setInterval(() => {
  if (!lastEvent) return;
  lastEvent.age = (lastEvent.age || 0) + 1;
  if ((lastEvent.cls === "found" && lastEvent.age >= 2) || lastEvent.age >= 90) {
    lastEvent = null;
    renderStatus();
  }
}, 60000);

invoke("get_site_visible").then((v) => { siteVisible = v; renderStatus(); });

// ───────── обновления ─────────
async function checkUpdate({ notify = false, manual = false } = {}) {
  const msg = $("#update-msg");
  if (manual) msg.textContent = "Проверяю…";
  try {
    const r = await invoke("check_update", { notify });
    if (r.available) {
      $("#update-text").textContent = `Доступна версия ${r.version} (у тебя ${r.current})`;
      $("#update-btn").disabled = false;
      $("#update-btn").textContent = "Обновить";
      $("#update-banner").hidden = false;
      msg.textContent = `Есть версия ${r.version}`;
    } else if (manual) {
      msg.textContent = `У тебя последняя версия (${r.current}) ✓`;
    }
  } catch (e) {
    if (manual) msg.textContent = String(e);
  }
}

$("#update-btn").addEventListener("click", async () => {
  const btn = $("#update-btn");
  btn.disabled = true;
  btn.textContent = "Качаю…";
  try {
    await invoke("install_update"); // после установки клиент перезапустится сам
  } catch (e) {
    $("#update-text").textContent = String(e);
    btn.disabled = false;
    btn.textContent = "Ещё раз";
  }
});

listen("update-progress", ({ payload: { got, total } }) => {
  $("#update-btn").textContent = total ? `${Math.round((got / total) * 100)}%` : `${(got / 1048576).toFixed(1)} МБ`;
});
listen("check-update", () => {
  showTab("settings");
  checkUpdate({ manual: true });
});
$("#check-update").addEventListener("click", () => checkUpdate({ manual: true }));

// ───────── события ─────────
listen("config-changed", () => loadConfig());
listen("room-detected", (e) => {
  if (!CONFIG.auto_open_lobby) return;
  showTab("lobby");
  loadLobby(e.payload);
});

loadConfig().then((cfg) => {
  if (cfg.auto_update) setTimeout(() => checkUpdate({ notify: true }), 3000);
  if (!cfg.api_key) showTab("settings");
  else if (cfg.nickname) loadProfile(cfg.nickname);
});
