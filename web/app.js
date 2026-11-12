"use strict";
(() => {
  const FIXTURE = new URLSearchParams(location.search).has("fixture");
  const $ = (s) => document.querySelector(s);
  const $$ = (s) => [...document.querySelectorAll(s)];
  let state = null, fixtureData = null, fixtureTx = null, fixtureLaunch = null, meta = null;
  let filter = localStorage.getItem("adash.filter") || "all";
  let selName = null;
  // Chat overlay state
  const ov = { open: false, name: null, tab: "info", tx: null, pending: [], held: null, openGroups: new Set(), lastInfoHtml: "", lastLogHtml: "", polling: false };

  const STATE_KO = { asking: "질문 대기", working: "작업 중", done: "완료", waiting: "승인 대기", blocked: "막힘", idle: "유휴", error: "오류로 멈춤", auth: "로그인 필요", trust: "폴더 신뢰 필요", approval: "승인 필요" };
  const STATUS_KO = { queued: "대기열", held_possible_draft: "보류(초안 의심)", delivered: "전달됨", delivery_unconfirmed: "전달 미확인", answered: "답변됨", relay_held: "답변 보류", relayed: "회신 완료", escalated: "에스컬레이션", failed: "실패" };
  const HELD_KO = { trust: "폴더 신뢰 필요", draft: "입력창 초안", working: "작업 중", asking: "질문 대기", unreadable: "입력창 확인 불가", network: "네트워크 대기" };
  // Message/answer channels recorded per interaction.
  const CHANNEL_KO = { terminal: "터미널", "term-send": "터미널", "terminal-answer": "터미널", dashboard: "대시보드" };
  const chanKo = (c) => CHANNEL_KO[c] || c;
  const answeredVia = (m) => (m?.answerChannel ? ` · ${chanKo(m.answerChannel)}` : "");
  // Review replies parked for a requester (`relay_held`), keyed by requester name.
  const heldRelays = (name) => (state?.threads || []).filter((t) => t.status === "relay_held" && (!name || t.from === name));
  const TYPE_SHORT = { omp: "omp", antigravity: "agy", agy: "agy", claude: "cc", codex: "cdx" };
  const CACHE_KO = { warm: "캐시 따뜻", cooling: "캐시 식는 중", cold: "캐시 차가움" };
  const MODE_KO = { line: "한 줄", paste: "붙여넣기", file: "파일" };

  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const enc = encodeURIComponent;
  const now = () => (FIXTURE && state ? state.generatedAt : Date.now());
  const fmtMin = (m) => (m == null || isNaN(m) ? "" : m < 60 ? `${Math.round(m)}분` : m < 1440 ? `${(m / 60).toFixed(1)}시간` : `${(m / 1440).toFixed(1)}일`);
  const fmtClock = (ms) => { const s = Math.max(0, Math.floor(ms / 1000)); const h = Math.floor(s / 3600), mm = String(Math.floor((s % 3600) / 60)).padStart(2, "0"), ss = String(s % 60).padStart(2, "0"); return h ? `${h}:${mm}:${ss}` : `${mm}:${ss}`; };
  const stateKo = (s) => STATE_KO[s] || (s ? `${s} (승인 대기)` : "?");
  // `asking` (an open ask/permission prompt) blocks on me even while the session record still shows `working`.
  // `error` = stopped on a provider/API error (lastError); it blocks on me like an open question.
  const kindOf = (a) => (a.state === "asking" || a.state === "trust" || a.state === "approval" || a.ask ? "asking" : a.state === "error" || a.state === "auth" ? "error" : a.needsHuman && !a.snoozedUntil ? "waiting" : a.state === "working" ? "working" : "idle");
  const errLabel = (e) => (e ? `오류${e.status ? ` ${e.status}` : ""}` : "오류");
  const errTip = (e) => (e ? [`${errLabel(e)}${e.provider || e.model ? ` · ${[e.provider, e.model].filter(Boolean).join(" / ")}` : ""}`, e.message || "", e.at ? new Date(e.at).toLocaleString("ko-KR") : ""].filter(Boolean).join("\n") : "제공자 API 오류로 멈춤");
  // Retry (v19): the label comes from the backend (`agent.retry.label`, e.g. how that harness retries);
  // omp's default is its F5 retry.
  const retryLabel = (s) => s?.retry?.label || (String(s?.agentType || "").toLowerCase() === "omp" ? "F5 재시도" : "재시도");
  const retryTip = (s) => `${retryLabel(s)}${s?.retry?.how ? ` · ${s.retry.how}` : ""}\n같은 요청을 다시 보냅니다. 결과는 토스트로 알려 줍니다.`;
  const retryMsg = (name, r) => (r?.resumed || r?.state === "working" ? `${name}: 재개됨` : `${name}: 아직 오류${r?.error?.status || r?.lastError?.status ? ` ${r.error?.status || r.lastError?.status}` : ""}${r?.error?.message || r?.message ? ` · ${String(r.error?.message || r.message).slice(0, 80)}` : ""}`);
  async function retryAgent(name, btn) {
    if (!name) return;
    if (btn) { btn.disabled = true; btn.dataset.label = btn.textContent; btn.textContent = "재시도 중…"; }
    try { toast(retryMsg(name, await api("POST", `/api/agents/${enc(name)}/retry`, {}, { quiet: true })), 4000); }
    catch (e) { toast(`오류: ${e.message}`, 5000); }
    finally { if (btn?.isConnected) { btn.disabled = false; btn.textContent = btn.dataset.label; } if (!FIXTURE) refresh(); else render(); }
  }
  // 문제 모두 해결: provider errors are retried; agents at a sign-in screen are restarted on the
  // same session (the server does it only when the harness is signed in elsewhere).
  function retryAllErrors() {
    const ts = (state?.agents || []).filter((a) => ["error", "auth"].includes(a.state) && !a.snoozedUntil);
    const errs = ts.filter((a) => a.state === "error"), auths = ts.filter((a) => a.state === "auth");
    const warn = closeWarnings(auths.map((a) => a.name));
    const li = (a) => `<li><b>${esc(a.name)}</b> · ${a.state === "auth" ? L2("로그인 필요 → 다시 시작 (다른 창에서 로그인돼 있을 때)", "sign-in needed → restart (when signed in elsewhere)") : L2("오류 → 재시도", "error → retry")}</li>`;
    dialog(`<h3>${L2(`문제 ${ts.length}개 모두 해결`, `Fix all ${ts.length} problems`)}</h3><ul class="small">${ts.map(li).join("")}</ul>${errs.length ? `<p class="muted small">${L2("오류: 네트워크(VPN·프록시·회선)를 바꾸지 않았다면 같은 오류가 다시 날 수 있습니다.", "Errors: unless the network changed, the same error may come back.")}</p>` : ""}${warn.length ? `<div class="close-warn"><b>${L2("주의", "Note")}</b><ul>${warn.join("")}</ul></div>` : ""}`, L2("모두 해결", "Fix all"), async () => {
      try {
        const r = await api("POST", "/api/fix-all", warn.length ? { confirmWorking: true, discardDraft: true } : {});
        const res = r?.results || [];
        const ok = (k) => res.filter((x) => x.action === k && (k === "retry" ? x.resumed || x.state === "working" : x.ok)).length;
        const left = res.filter((x) => !(x.action === "retry" ? x.resumed || x.state === "working" : x.ok));
        const why = (x) => x.reason === "not_signed_in" ? L2(`${x.name}: 아직 아무 창에서도 로그인하지 않았습니다`, `${x.name}: not signed in anywhere yet`) : x.action === "retry" ? retryMsg(x.name, x) : `${x.name}: ${CLOSE_WHY[x.reason] || x.reason}`;
        toast(`${L2("재개", "resumed")} ${ok("retry")} · ${L2("다시 시작", "restarted")} ${ok("restart")} · ${L2("남은 문제", "left")} ${left.length}${left.length ? `\n${left.map(why).join("\n")}` : ""}`, 9000);
        // nobody signed in yet: sign in once, in the first such agent's terminal
        const first = left.find((x) => x.reason === "not_signed_in");
        if (first) { toast(L2(`먼저 한 창(${first.name})에서 로그인하세요. 그다음 다시 '문제 모두 해결'`, `Sign in once (${first.name}), then fix all again`), 9000); openChat(first.name); setTimeout(() => setView("terminal"), 400); }
      } catch { /* toasted */ }
      if (!FIXTURE) refresh(); else render();
    });
  }
  // `cache` was a string ("warm|cooling|cold") and is now {ttlSec, expiresAt, state} (A4).
  const cacheState = (s) => (typeof s?.cache === "string" ? s.cache : s?.cache?.state || null);
  // Cache TTL is per harness + provider (omp/claude+Anthropic 3600 s, OpenAI 2400 s, z.ai·Gemini 300 s); `basis` is the
  // backend's one-line reason ("omp+Anthropic: 1시간 캐시(cacheRetention auto), 실측 약 60분").
  const cacheBasis = (s) => { const c = s && typeof s.cache === "object" ? s.cache : null; if (!c) return ""; const ttl = c.ttlSec >= 3600 && c.ttlSec % 3600 === 0 ? `${c.ttlSec / 3600}시간` : c.ttlSec >= 60 ? `${Math.round(c.ttlSec / 60)}분` : `${c.ttlSec}초`; return [c.ttlSec ? `캐시 수명 ${ttl}` : "", c.basis ? raw(c.basis) : ""].filter(Boolean).join(" · "); };
  const cacheExp = (s) => (s && typeof s.cache === "object" && s.cache ? s.cache.expiresAt || null : null);
  // Wall clock that also ticks in the fixture (whose `now` is frozen at generatedAt).
  const nowTick = () => (FIXTURE && state ? state.generatedAt + (Date.now() - loadedAt) : Date.now());
  let soonSec = Math.max(10, +localStorage.getItem("adash.soonSec") || 120);
  const soonCold = (s) => { const e = cacheExp(s); if (!e || cacheState(s) === "cold") return false; const d = e - nowTick(); return d > 0 && d <= soonSec * 1000; };
  const decisionsOf = (s) => (Array.isArray(s?.decisions) ? s.decisions : []);
  const waited = (a) => a.waitedMin ?? (a.stateSince ? (now() - a.stateSince) / 60000 : 0);
  const sinceMin = (a) => (a.stateSince ? Math.max(0, (now() - a.stateSince) / 60000) : 0);
  const agentBy = (n) => state?.agents.find((a) => a.name === n);
  // Write innerHTML only when it changed, so polling does not swap nodes under the pointer.
  // While an editable element inside the target has focus, the update is deferred until focus leaves,
  // so typing (value, selection, caret, IME composition) is never interrupted by a re-render.
  const isEditable = (el) => !!el && el instanceof Element && (el.matches("input:not([type=checkbox]):not([type=radio]):not([type=button]):not([type=submit]), textarea, select") || el.isContentEditable);
  const deferred = new Set();
  const H = (sel) => ({ set innerHTML(v) {
    const el = $(sel);
    if (el._h === v) { el._pending = undefined; deferred.delete(el); return; }
    const a = document.activeElement;
    if (a && el.contains(a) && isEditable(a)) { el._pending = v; deferred.add(el); return; }
    el.innerHTML = v; el._h = v; el._pending = undefined; deferred.delete(el);
  } });
  // Flush deferred updates once focus has left the region that was being edited.
  document.addEventListener("focusout", () => setTimeout(() => {
    for (const el of [...deferred]) {
      const a = document.activeElement;
      if (a && el.contains(a) && isEditable(a)) continue;
      el.innerHTML = el._pending; el._h = el._pending; el._pending = undefined; deferred.delete(el);
    }
  }, 0), true);

  function toast(msg, ms = 4500) {
    const t = $("#toast"); t.textContent = msg; t.hidden = false;
    clearTimeout(toast.h); toast.h = setTimeout(() => (t.hidden = true), ms);
  }

  // ---------- font settings (persisted in localStorage) ----------
  const FONT_DEFAULTS = { font: "system", custom: "", mono: "ui-monospace, monospace", chat: 16, ui: 100 };
  let fontCfg = { ...FONT_DEFAULTS, ...(() => { try { return JSON.parse(localStorage.getItem("adash.font") || "{}"); } catch { return {}; } })() };
  const cssFamily = (s) => s.split(",").map((f) => f.trim()).filter(Boolean).map((f) => (/^['"]|^[a-z-]+$/i.test(f) ? f : `"${f.replace(/"/g, "")}"`)).join(", ");
  function applyFont(save = true) {
    fontCfg.chat = Math.min(26, Math.max(12, +fontCfg.chat || 16));
    fontCfg.ui = Math.min(170, Math.max(80, +fontCfg.ui || 100));
    const base = 'Pretendard, system-ui, "IBM Plex Sans KR", "Noto Sans KR", sans-serif';
    const fam = fontCfg.font === "system" ? base : fontCfg.font === "custom" ? (fontCfg.custom.trim() ? `${cssFamily(fontCfg.custom)}, ${base}` : base) : `${fontCfg.font}, ${base}`;
    const r = document.documentElement.style;
    // "system" / the default code font leave the theme's --font-ui / --font-mono in charge.
    if (fontCfg.font === "system") r.removeProperty("--font"); else r.setProperty("--font", fam);
    if (fontCfg.mono === FONT_DEFAULTS.mono) r.removeProperty("--mono"); else r.setProperty("--mono", fontCfg.mono);
    r.setProperty("--chat-fs", `${fontCfg.chat}px`);
    r.setProperty("--ui-zoom", String(fontCfg.ui / 100));
    $("#fp-font").value = fontCfg.font; $("#fp-custom").value = fontCfg.custom; $("#fp-mono").value = fontCfg.mono;
    $("#fp-custom-row").hidden = fontCfg.font !== "custom";
    $("#fp-chat").value = fontCfg.chat; $("#fp-chat-v").textContent = `${fontCfg.chat}px`;
    $("#fp-ui").value = fontCfg.ui; $("#fp-ui-v").textContent = `${fontCfg.ui}%`;
    if (save) localStorage.setItem("adash.font", JSON.stringify(fontCfg));
    if (typeof measureTkSlot === "function") requestAnimationFrame(measureTkSlot);
  }
  const bumpChat = (d) => { fontCfg.chat += d; applyFont(); };
  $("#fp-font").addEventListener("change", (e) => { fontCfg.font = e.target.value; applyFont(); if (fontCfg.font === "custom") $("#fp-custom").focus(); });
  $("#fp-custom").addEventListener("input", (e) => { fontCfg.custom = e.target.value; applyFont(); });
  $("#fp-mono").addEventListener("change", (e) => { fontCfg.mono = e.target.value; applyFont(); });
  $("#fp-chat").addEventListener("input", (e) => { fontCfg.chat = +e.target.value; applyFont(); });
  $("#fp-ui").addEventListener("input", (e) => { fontCfg.ui = +e.target.value; applyFont(); });
  $("#fp-reset").addEventListener("click", () => { fontCfg = { ...FONT_DEFAULTS }; applyFont(); });
  $("#fp-soon").value = soonSec;
  $("#fp-soon").addEventListener("change", (e) => { soonSec = Math.max(10, +e.target.value || 120); e.target.value = soonSec; localStorage.setItem("adash.soonSec", soonSec); if (state) render(); });
  // Enter to send (per device): default on for desktop, off for touch devices (phone layout or coarse pointer),
  // where Enter inserts a newline and only the 보내기 button sends. Ctrl/Cmd+Enter always sends.
  const touchMq = matchMedia("(max-width: 767px), (pointer: coarse)");
  const enterSends = () => { const v = localStorage.getItem("adash.enterSend"); return v ? v === "on" : !touchMq.matches; };
  function applyEnterMode() {
    const on = enterSends(), ta = $("#composer-text");
    $("#fp-enter").checked = on;
    ta.setAttribute("enterkeyhint", on ? "send" : "enter");
    ta.placeholder = on ? "메시지 입력 · Enter 보내기 · Shift+Enter 줄바꿈 · Ctrl+Enter 강제 전송 · 파일 붙여넣기/끌어다 놓기" : "메시지 입력 · Enter 줄바꿈 · 보내기는 버튼으로 · 파일 붙여넣기";
    $$(".ask-text").forEach((x) => x.setAttribute("enterkeyhint", on ? "send" : "done"));
  }
  $("#fp-enter").addEventListener("change", (e) => { localStorage.setItem("adash.enterSend", e.target.checked ? "on" : "off"); applyEnterMode(); });
  touchMq.addEventListener("change", applyEnterMode);
  applyEnterMode();
  // UI language (v21): saved per device; switching reloads so every label renders in the new language.
  $("#fp-lang").value = window.ADASH_LANG || "ko";
  $("#fp-lang").addEventListener("change", (e) => {
    localStorage.setItem("adash.lang", e.target.value);
    const u = new URL(location.href); u.searchParams.delete("lang"); location.replace(u);
  });
  document.addEventListener("click", (e) => {
    const p = $("#font-panel");
    if (e.target.closest(".font-btn")) { p.hidden = !p.hidden; return; }
    if (!p.hidden && !e.target.closest("#font-panel")) p.hidden = true;
  }, true);
  // Shared popover placement (v44): fixed to the viewport, below the anchor when it fits, else above, else the
  // taller side with max-height + inner scroll; clamped left/right. Phones: a bottom sheet. Closed on resize /
  // outside scroll (closePops).
  // Popovers carry the board zoom (보드 배율, CSS `zoom` on .pop): left/top/max-height are in the popover's own
  // zoomed px, so viewport coordinates are divided by that zoom (else a 130% board put them off-screen).
  // An element in fullscreen hides everything outside it: the popover moves into the fullscreen element.
  function placePop(el, anchor, { align = "left" } = {}) {
    const fs = document.fullscreenElement;
    if (fs && !fs.contains(el)) { el._home ??= el.parentElement; fs.appendChild(el); }
    else if (!fs && el._home && el.parentElement !== el._home) el._home.appendChild(el);
    el.classList.add("placed");
    el.style.maxHeight = ""; el.style.left = el.style.top = el.style.right = el.style.bottom = "";
    if (isMobile()) { el.classList.add("sheet"); return; }
    el.classList.remove("sheet");
    const z = el.currentCSSZoom || parseFloat(getComputedStyle(el).zoom) || 1;
    const r = anchor.getBoundingClientRect(), M = 8, vw = innerWidth, vh = innerHeight;
    const box = el.getBoundingClientRect(), w = Math.min(box.width, vw - 2 * M), h = box.height;
    const below = vh - r.bottom - M - 4, above = r.top - M - 4;
    let top, maxH = null;
    if (h <= below) top = r.bottom + 4;
    else if (h <= above) top = r.top - 4 - h;
    else if (below >= above) { top = r.bottom + 4; maxH = below; }
    else { maxH = above; top = M; }
    let left = align === "right" ? r.right - w : r.left;
    left = Math.max(M, Math.min(left, vw - w - M));
    Object.assign(el.style, { left: `${left / z}px`, top: `${Math.max(M, top) / z}px`, maxHeight: maxH != null ? `${Math.max(120, maxH) / z}px` : `calc((100vh - ${2 * M}px) / ${z})` });
  }
  const POP_SEL = ".pop:not([hidden]), #proj-menu:not([hidden]), #model-pop:not([hidden])";
  function closePops() {
    $$(".pop").forEach((p) => { p.hidden = true; });
    $$("[data-pop]").forEach((b) => b.classList.remove("on"));
    if (typeof closeModelPop === "function" && $("#model-pop") && !$("#model-pop").hidden) closeModelPop();
  }
  addEventListener("resize", () => { if (document.querySelector(POP_SEL)) closePops(); });
  // A scroll anywhere but inside the open popover moves its anchor: close instead of drifting.
  document.addEventListener("scroll", (e) => { const open = document.querySelector(POP_SEL); if (open && !(e.target instanceof Node && open.contains(e.target))) closePops(); }, true);
  // Popover menus ([data-pop] buttons → #id .pop): one open at a time; any click outside, or on a menu item,
  // closes them. The 보기 popover and the model picker stay open while their controls are used.
  document.addEventListener("click", (e) => {
    const pb = e.target.closest("[data-pop]");
    const target = pb && document.getElementById(pb.dataset.pop);
    const keep = !pb && e.target.closest(".view-pop, #model-pop, .snz-custom");
    const opening = target && target.hidden;
    if (!keep) $$(".pop").forEach((p) => { p.hidden = true; });
    $$("[data-pop]").forEach((b) => b.classList.remove("on"));
    if (opening) { target.hidden = false; pb.classList.add("on"); placePop(target, pb, { align: pb.closest(".ov-head") ? "left" : "right" }); }
    const rt = e.target.closest("[data-rtab]");
    if (rt) { recentTab = rt.dataset.rtab; localStorage.setItem("adash.recentTab", recentTab); renderRecent(); }
    const lk = e.target.closest("[data-links]");
    if (lk) setLinkMode(lk.dataset.links);
    if (e.target.closest("#search-btn")) openPalette();
    // Links, chips and thumbnails act on their own and never also pick the ask option they sit in.
    const sk = e.target.closest(".skchip");
    if (sk) { e.preventDefault(); e.stopPropagation(); return sk.dataset.skInsert ? insertSkill(sk.dataset.skInsert, sk.dataset.skMid) : undefined; }
    // Retry buttons sit inside rows/cards that open the chat on click: handle them here and stop there.
    const rb = e.target.closest("[data-retry], [data-retry-all]");
    if (rb) { e.preventDefault(); e.stopPropagation(); return rb.dataset.retryAll != null ? retryAllErrors() : retryAgent(rb.dataset.retry, rb); }
    // Restart-when-idle reservation: the ↻ mark (in a row that opens the chat) and the info panel button cancel it.
    const oc = e.target.closest("[data-od-cancel]");
    if (oc) { e.preventDefault(); e.stopPropagation(); return odCancel(oc.dataset.odCancel); }
    // "다시 열기" on offline rows/cards: act here so the row's own click (open chat) doesn't also fire.
    const ro = e.target.closest("[data-reopen]");
    if (ro) { e.preventDefault(); e.stopPropagation(); return reopenAgent(ro.dataset.reopen); }
    // "이어서 열기" on a past session found by search (identity = session file)
    const rf = e.target.closest("[data-reopen-file]");
    if (rf) { e.preventDefault(); e.stopPropagation(); return reopenSession(rf.dataset.reopenFile, rf.dataset.label); }
    if (e.target.closest("[data-sess-back]")) { e.preventDefault(); return openSessions(); }
    const th = e.target.closest(".ask-thumb");
    if (th) { e.preventDefault(); e.stopPropagation(); return openViewer(`<img src="${esc(th.src)}" alt="${esc(th.dataset.viewPath)}">`); }
    const al = e.target.closest("#ask-card .ask-opt a, #ask-card .ask-q a");
    if (al) {
      e.stopPropagation();
      if (al.classList.contains("pathlink")) { e.preventDefault(); openPath(al, e.altKey); }
    }
  }, true);
  // Ctrl+wheel over the chat changes the chat font size instead of zooming the page.
  document.addEventListener("wheel", (e) => {
    if (!e.ctrlKey || !e.target.closest(".ov")) return;
    e.preventDefault();
    bumpChat(e.deltaY < 0 ? 0.5 : -0.5);
  }, { passive: false });
  applyFont(false);

  // ---------- fixture stubs ----------
  function fixtureApi(method, path, body) {
    console.log("[fixture] would send", method, path, method === "GET" ? "" : "X-Adash: 1", body === undefined ? "" : JSON.stringify(body));
    if (method !== "GET" && !path.endsWith("/stat")) toast(`[fixture] ${method} ${path}${body === undefined ? "" : " " + JSON.stringify(body)}`.slice(0, 300));
    if (method === "GET" && path === "/api/meta") return structuredClone(fixtureData.meta);
    if (path === "/api/request-names") {
      const amb = fixtureData.agents.filter((a) => a.ambiguous);
      return { queued: amb.filter((a) => a.state !== "working").map((a) => a.name), skipped: amb.filter((a) => a.state === "working").map((a) => ({ name: a.name, reason: "working" })) };
    }
    let m = path.match(/^\/api\/agents\/([^/]+)\/transcript(?:\?(.*))?$/);
    if (m) {
      const name = decodeURIComponent(m[1]), q = new URLSearchParams(m[2] || "");
      // ?pending=NAME: that agent looks like a fresh session for its first 3 polls (source "pending", no session
      // file yet), then maps to its real transcript.
      const pn = new URLSearchParams(location.search).get("pending");
      if (pn === name && q.get("source") !== "terminal" && ((fixtureApi.pendingPolls ||= 0), fixtureApi.pendingPolls++ < 3))
        return { source: "pending", hint: "첫 메시지를 보내면 대화가 여기에 표시됩니다", terminalAvailable: true, cursor: `pending:${fixtureApi.pendingPolls}`, replace: true, messages: [{ id: "pending:launch", role: "user", status: "pending", text: "Read the spec and start", ts: fixtureData.generatedAt }] };
      if (q.get("source") === "terminal") return { source: "terminal", messages: [{ id: "snap", role: "system", text: `$ agy --model gemini-3-pro\n> ${name}: model retrain\n  epoch 13/200  loss 3.37e-3\n\n  esc to interrupt` }] };
      const a = fixtureData.agents.find((x) => x.name === name);
      let tx = fixtureTx[name];
      if (!tx) {
        const msgs = [];
        if (a?.prompt) msgs.push({ id: "p", role: "user", text: a.prompt, ts: a.stateSince - 600000, tool: null });
        if (a?.lastMessage) msgs.push({ id: "l", role: "assistant", text: a.lastMessage, ts: a.stateSince, tool: null });
        tx = fixtureTx[name] = { agent: name, source: "omp-jsonl", mappingConfidence: "heuristic", cursor: msgs.length ? msgs[msgs.length - 1].id : null, state: a?.state, tool: a?.tool, messages: msgs };
      }
      const after = q.get("after");
      const i = after ? tx.messages.findIndex((x) => x.id === after) : -1;
      const messages = tx.source === "terminal" ? tx.messages : tx.messages.slice(i + 1);
      return structuredClone({ ...tx, messages, cursor: tx.messages.length ? tx.messages[tx.messages.length - 1].id : tx.cursor });
    }
    m = path.match(/^\/api\/agents\/([^/]+)\/send$/);
    if (m) {
      const name = decodeURIComponent(m[1]);
      const a = fixtureData.agents.find((x) => x.name === name);
      if (!body.force && a?.state === "working") return { ok: true, delivered: false, held: "working", mode: null, file: null };
      if (!body.force && (fixtureTx._heldDraft || []).includes(name)) return { ok: true, delivered: false, held: "possible_draft", mode: null, file: null };
      // "/btw …": recorded as a btw question (pending → answered) instead of a normal user message.
      if (/^\s*\/btw\b/.test(body.text)) {
        const q = body.text.replace(/^\s*\/btw\s*/, ""), id = `fx-btw-${Date.now().toString(36)}`, tx = (fixtureTx[name] ||= { messages: [] });
        const a0 = fixtureData.agents.find((x) => x.name === name);
        setTimeout(() => { tx.messages.push({ id: `${id}q`, role: "user", kind: "btw", btwId: id, status: "pending", ts: fixtureData.generatedAt, text: q, tool: null }); if (a0) a0.btw = { open: true, btwId: id }; refresh(); }, 500);
        setTimeout(() => { const m = tx.messages.find((x) => x.id === `${id}q`); if (m) m.status = "answered"; tx.messages.push({ id: `${id}a`, role: "assistant", kind: "btw", btwId: id, ts: fixtureData.generatedAt, text: `(demo) A side answer to '${q}'.`, tool: null }); refresh(); }, 1600);
        return { ok: true, delivered: true, mode: "line", kind: "btw", btwText: q };
      }
      const mode = body.text.includes("\n") ? (body.text.length > 2000 ? "file" : "paste") : "line";
      const tx = fixtureTx[name];
      const imgs = body.images || [], fls = body.files || [];
      const full = [body.text, ...imgs.map((p, k) => `[첨부 이미지 ${k + 1}] ${p} — 읽기 도구로 이 이미지를 열어 확인하세요.`), ...fls.map((p, k) => `[첨부 파일 ${k + 1}] @${p}`)].filter((s) => s && s.trim()).join("\n");
      if (tx && tx.source !== "terminal") setTimeout(() => tx.messages.push({ id: `u${Date.now()}`, role: "user", text: full, ts: now(), tool: null, attachments: [...imgs.map((p) => ({ path: p, kind: "image" })), ...fls.map((p) => ({ path: p, kind: "file", size: fixtureSizes.get(p) ?? null, name: fixtureSizes.get(`name:${p}`) ?? null }))] }), 1500);
      return { ok: true, delivered: true, held: null, mode, file: mode === "file" ? `~/.local/share/adash/sends/${name}.md` : null };
    }
    m = path.match(/^\/api\/agents\/([^/]+)\/usage\/hourly(?:\?.*)?$/);
    if (m) return fixtureHourly(decodeURIComponent(m[1]));
    m = path.match(/^\/api\/agents\/([^/]+)\/stat$/);
    // Fixture: absolute/home paths exist unless they say missing/nope; bare relative paths only resolve via a base.
    if (m) return { results: body.paths.map((p) => ({ path: p, exists: /^(~\/|\/|file:\/\/\/)/.test(p) && !/missing|nope/.test(p), dir: p.endsWith("/") || !/\.[A-Za-z0-9]{1,8}$/.test(p) })) };
    if (/^\/api\/agents\/[^/]+\/open$/.test(path)) return { ok: true, app: /\.md$/i.test(body.path) ? "markdown-editor" : "xdg-open" };
    m = path.match(/^\/api\/agents\/([^/]+)\/models$/);
    if (m && method === "GET") {
      // Mirrors the server: omp both (session only, Alt+P temporary picker), Claude Code both (and the choice
      // becomes its default), Codex both (session only), Antigravity neither.
      const a = fixtureData.agents.find((x) => x.name === decodeURIComponent(m[1]));
      const h = /antigravity|agy/.test(a?.agentType || "") ? "agy" : /claude/.test(a?.agentType || "") ? "claude" : /codex/.test(a?.agentType || "") ? "codex" : "omp";
      const E = ["low", "medium", "high"], EX = ["minimal", "low", "medium", "high", "xhigh"];
      const models = h === "claude" ? [["opus", "Opus (claude-opus-5-5)", E.concat("max")], ["sonnet", "Sonnet (claude-sonnet-5)", E], ["haiku", "Haiku (claude-haiku-4-5)", []], ["fable", "Fable", E]]
        : h === "codex" ? [["gpt-5.2-codex", "GPT-5.2 Codex", EX], ["gpt-5.2", "GPT-5.2", EX], ["gpt-5.1-codex-mini", "GPT-5.1 Codex mini", ["medium", "high"]]]
        : h === "omp" ? [["anthropic/claude-opus-5-5", "Claude Opus 5.5", EX], ["anthropic/claude-sonnet-5", "Claude Sonnet 5", EX], ["openai/gpt-5.2-codex", "GPT-5.2 Codex", EX]] : [];
      const curModel = h === "claude" ? "opus" : h === "codex" ? "gpt-5.2-codex" : h === "omp" ? "anthropic/" + (a?.usage?.model || a?.model || "claude-opus-5-5") : null;
      const cur = models.find((x) => x[0] === curModel);
      return { harness: a?.agentType || "omp", current: { model: curModel, effort: a?.usage?.thinking || (cur?.[2].length ? "medium" : null) }, models: models.map(([id, label, efforts]) => ({ id, label, efforts })), efforts: cur?.[2] || [],
        canSetModel: h !== "agy", canSetEffort: h !== "agy", persistsDefault: h === "claude",
        how: h === "agy" ? "Antigravity: pick the model in its own window (terminal → model menu)." : null };
    }
    m = path.match(/^\/api\/agents\/([^/]+)\/model$/);
    if (m && method === "POST") {
      const a = fixtureData.agents.find((x) => x.name === decodeURIComponent(m[1]));
      const fail = new URLSearchParams(location.search).get("modelerr");
      return new Promise((res, rej) => setTimeout(() => {
        if (fail || (a && a.state === "working")) { const e = new Error(fail ? "the harness did not confirm the switch" : "working: wait until it finishes"); e.status = fail ? 502 : 409; return rej(e); }
        if (a?.draft?.text) { const e = new Error("입력창에 보내지 않은 글이 있습니다"); e.status = 409; return rej(e); }
        if (a && body.model) { const id = String(body.model).split("/").pop(); a.model = id; if (a.usage) a.usage.model = id; }
        if (a && body.effort) { a.usage = a.usage || {}; a.usage.thinking = body.effort; }
        res({ ok: true, sent: body });
      }, 900));
    }
    // Fixture: harness updated under some sessions; restart = the session comes back current.
    if (path === "/api/restart-outdated" && method === "POST") {
      fixtureApi.odPend ||= new Set();
      if (body?.cancel) { (body.names || [...fixtureApi.odPend]).forEach((n) => fixtureApi.odPend.delete(n)); return { pending: [...fixtureApi.odPend] }; }
      const results = fixtureData.agents.filter((a) => a.outdated && (!body?.names || body.names.includes(a.name)) && (!body?.harness || a.agentType === body.harness)).map((a) => {
        const p = odPlan(a, !!body?.whenIdle);
        if (p === "now") { delete a.outdated; return { name: a.name, action: "restarted" }; }
        if (p === "later") { fixtureApi.odPend.add(a.name); return { name: a.name, action: "later" }; }
        return { name: a.name, action: "skip", reason: p };
      });
      setTimeout(refresh, 50);
      return { results, pending: [...fixtureApi.odPend] };
    }
    m = path.match(/^\/api\/agents\/([^/]+)\/snooze$/);
    if (m && method === "POST") {
      const a = fixtureData.agents.find((x) => x.name === decodeURIComponent(m[1]));
      if (a) { a.snoozedUntil = body?.minutes ? now() + body.minutes * 60000 : null; a.snoozedAt = a.snoozedUntil ? now() : null; }
      setTimeout(refresh, 50);
      return { ok: true, snoozedUntil: a?.snoozedUntil ?? null };
    }
    m = path.match(/^\/api\/agents\/([^/]+)\/(ack|draft)$/);
    if (m) {
      const a = fixtureData.agents.find((x) => x.name === decodeURIComponent(m[1]));
      if (a && m[2] === "ack") { if (method === "DELETE") { a.ackedAt = null; a.unread = true; } else { a.ackedAt = fixtureData.generatedAt; a.unread = false; } }
      if (a && m[2] === "draft") setTimeout(() => { a.draft = null; refresh(); }, 300);
      setTimeout(refresh, 50);
      return { ok: true };
    }
    if (path === "/api/reports") {
      fixtureApi.reports ||= [
        { id: "week-2026-W43", period: "week", title: "주간 2026-W43", from: "2026-10-19", to: "2026-10-25", generatedAt: fixtureData.generatedAt - 2 * 86400000, hasPdf: true, headline: { messages: 412, activeHours: "46.5h", agents: 27, turns: 1630 } },
        { id: "month-2026-09", period: "month", title: "월간 2026-09", from: "2026-09-01", to: "2026-09-30", generatedAt: fixtureData.generatedAt - 27 * 86400000, hasPdf: true, headline: { messages: 1580, activeHours: "171h", agents: 41, turns: 6120 } },
      ];
      if (method === "POST") {
        const id = `${body.period}-${body.from || "now"}`;
        setTimeout(() => fixtureApi.reports.unshift({ id, period: body.period, title: `${PERIOD_KO[body.period] || body.period} (지금 만듦)`, from: body.from || "2026-10-26", to: body.to || "2026-10-28", generatedAt: fixtureData.generatedAt, hasPdf: true, headline: { messages: 120, activeHours: "12.0h" } }), 600);
        return { id };
      }
      return { reports: fixtureApi.reports };
    }
    m = path.match(/^\/api\/sessions\/sess-([^/]+)\/transcript/);
    if (m) return structuredClone(fixtureTx[decodeURIComponent(m[1])] || { messages: [] });
    if (method === "GET" && path.startsWith("/api/magi/roles")) {
      const src = fixtureData.agents.find((x) => x.name === decodeURIComponent(new URLSearchParams(path.split("?")[1]).get("source")));
      const same = fixtureData.agents.filter((x) => x.project === src?.project && x.name !== src?.name);
      const critic = same.filter((x) => x.agentType === "codex" || /gpt/i.test(x.model || "")).map((x, i) => ({ name: x.name, default: i === 0 }));
      const proposer = same.filter((x) => x.agentType === "antigravity").map((x, i) => ({ name: x.name, default: i === 0 }));
      return { critic, proposer, missing: [...(critic.length ? [] : ["critic"]), ...(proposer.length ? [] : ["proposer"])] };
    }
    if (method === "POST" && path === "/api/magi") {
      // Drafted source → held; otherwise delivered. Mirrors RunState::view.
      const src = fixtureData.agents.find((x) => x.name === body.source);
      const held = src?.state === "working" ? "working" : src?.draft?.text ? "possible_draft" : null;
      const v = { id: `M${Date.now().toString(36)}`, source: body.source, kind: body.kind, mode: body.mode, status: held ? "held" : "delivered", phase: held ? "held" : "delivered", heldReason: held, createdAt: Date.now(), targets: body.targets.map((n) => ({ name: n, threadId: null, status: "not_asked" })) };
      (fixtureData.magiRuns ||= []).unshift(v);
      return structuredClone(v);
    }
    m = path.match(/^\/api\/magi\/([^/]+)\/(deliver|cancel)$/);
    if (m && method === "POST") {
      const r = (fixtureData.magiRuns || []).find((x) => x.id === m[1]);
      if (!r) return null;
      if (r.status === "delivered") { const e = new Error("already delivered"); e.status = 409; throw e; }
      if (m[2] === "cancel") Object.assign(r, { status: "cancelled", phase: "cancelled" });
      else if (r.heldReason === "working") {} // never into a working turn
      else Object.assign(r, { status: "delivered", phase: "delivered", heldReason: null, deliveredAt: Date.now() });
      return structuredClone(r);
    }
    if (method === "POST" && path === "/api/broadcast") return { results: body.targets.map((n, i) => (i % 3 === 2 ? { name: n, delivered: false, held: "possible_draft" } : { name: n, delivered: true, mode: "line" })) };
    if (method === "GET" && path.startsWith("/api/search")) {
      const q = new URLSearchParams(path.split("?")[1]).get("q") || "";
      const hits = [];
      for (const [agent, tx] of Object.entries(fixtureTx)) for (const m of tx.messages || []) {
        const i = (m.text || "").toLowerCase().indexOf(q.toLowerCase());
        if (q && i >= 0) hits.push({ agent, session: `sess-${agent}`, project: fixtureData.agents.find((a) => a.name === agent)?.project || "?", role: m.role, ts: m.ts, messageId: m.id, live: !!fixtureData.agents.find((a) => a.name === agent), snippet: `${m.text.slice(Math.max(0, i - 30), i)}<mark>${m.text.slice(i, i + q.length)}</mark>${m.text.slice(i + q.length, i + q.length + 60)}` });
      }
      return { hits: hits.slice(0, 50) };
    }
    if (method === "GET" && path === "/api/workspaces") return structuredClone(fixtureLaunch.workspaces);
    if (method === "GET" && path === "/api/launch/catalog") return structuredClone(fixtureLaunch.catalog);
    if (method === "GET" && path.startsWith("/api/capacity")) {
      if (path.includes("refresh=1")) fixtureLaunch.capacity.fetchedAt = fixtureData.generatedAt;
      return structuredClone(fixtureLaunch.capacity);
    }
    if (method === "POST" && ["/api/sessions", "/api/worktrees", "/api/projects"].includes(path)) {
      // Simulate the new pane showing up in /api/state a moment later.
      const handle = `t_${Date.now().toString(16).slice(-8)}`;
      const wt = fixtureLaunch.workspaces.projects.flatMap((r) => r.worktrees.map((w) => ({ r, w }))).find((x) => path === "/api/sessions" && x.w.path === body.path);
      const project = wt ? wt.r.name : path === "/api/worktrees" ? body.project : String(body.path || "새프로젝트").split("/").filter(Boolean).pop();
      const name = path === "/api/sessions" && body.name ? body.name : `${project}-${body.agent || "omp"}`;
      setTimeout(() => {
        fixtureData.agents.push({ name, nameSource: body.name ? "override" : "fallback", ambiguous: false, role: body.role || null, handle, project, worktreePath: wt?.w.path || body.path || "", agentType: body.agent || "omp", model: body.model || null, title: (body.prompt || "새 세션").split("\n")[0], state: "working", needsHuman: false, stateSince: fixtureData.generatedAt, waitedMin: 0, cache: null, tool: null, prompt: body.prompt || "", lastMessage: "", progress: null, snoozedUntil: null, note: "", ask: null });
        refresh();
      }, 1500);
      return { handle, ...(path === "/api/worktrees" ? { worktree: { path: `/fixture/${project}-wt/${body.name}`, branch: body.name } } : {}), command: body.agent || "omp" };
    }
    // Retry: SCOUT resumes, anything else stays in error (to show both toasts).
    const retryOne = (a) => {
      if (a.name === "SCOUT") { Object.assign(a, { state: "working", needsHuman: false, stateSince: fixtureData.generatedAt }); delete a.lastError; return { name: a.name, resumed: true, state: "working" }; }
      return { name: a.name, resumed: false, state: "error", error: a.lastError };
    };
    m = path.match(/^\/api\/agents\/([^/]+)\/btw(\/close)?$/);
    if (m && method === "POST") {
      const n = decodeURIComponent(m[1]), a = fixtureData.agents.find((x) => x.name === n);
      if (m[2]) { if (a) a.btw = { ...(a.btw || {}), open: false }; setTimeout(refresh, 0); return { ok: true }; }
      const r = fixtureApi("POST", `/api/agents/${m[1]}/send`, { text: `/btw ${body.text}`, force: true });
      return { ok: true, delivered: r.delivered };
    }
    if (method === "POST" && path === "/api/network/check") { const n = fixtureData.network; if (n) n.providers.forEach((p) => { p.checkedAt = Date.now(); }); return { ok: true, network: n }; }
    if (method === "PUT" && path === "/api/meta") { fixtureData.meta = { ...fixtureData.meta, ...body }; return { ok: true }; }
    m = path.match(/^\/api\/agents\/([^/]+)\/focus$/);
    if (m && method === "POST") return new URLSearchParams(location.search).get("focus") === "fail" ? { handle: "demo", opened: false, command: `tmux -L adash attach -t '=${decodeURIComponent(m[1])}'`, reason: "no terminal emulator found" } : { opened: true, terminal: "demo-term" };
    m = path.match(/^\/api\/projects\/([^/]+)\/(hide|unhide|close|archive|restore|reopen-all)$/);
    if (m && method === "POST") {
      { const pe = new URLSearchParams(location.search).get("projerr"); if (pe) { const e = new Error(pe); e.status = 500; e.data = { error: pe }; throw e; } }
      const pn = decodeURIComponent(m[1]), what = m[2];
      let pr = fixtureData.projects.find((x) => x.name === pn);
      if (!pr) { pr = { name: pn }; fixtureData.projects.push(pr); }
      const closeAll = () => fixtureData.agents.filter((a) => a.project === pn).map((a) => {
        const k = kindOf(a), risky = k === "working" || k === "asking" || !!a.draft?.text;
        if (risky && body.mode !== "force" && !(body.confirm || []).includes(a.name)) return { name: a.name, ok: false, reason: a.draft?.text ? "draft" : k };
        (fixtureApi.closed ||= new Map()).set(a.name, a); fixtureData.agents.splice(fixtureData.agents.indexOf(a), 1);
        return { name: a.name, ok: true };
      });
      const hp = (fixtureData.hiddenProjects ||= []);
      const setHidden = (v) => { const i = hp.findIndex((x) => x.name === pn); if (i >= 0) hp.splice(i, 1); if (v) hp.push({ name: pn, agents: fixtureData.agents.filter((a) => a.project === pn).length, archivedSessions: [], ...v }); fixtureData.agents.forEach((a) => { if (a.project === pn) a.hidden = !!v; }); };
      if (what === "close" || what === "archive") {
        const needs = fixtureData.agents.filter((a) => a.project === pn && (["working", "asking"].includes(kindOf(a)) || a.draft?.text) && body.mode !== "force" && !(body.confirm || []).includes(a.name)).map((a) => ({ name: a.name, why: a.draft?.text ? "draft" : kindOf(a), draft: a.draft?.text }));
        if (needs.length) { const e = new Error("confirm"); e.status = 409; e.data = { reason: "confirm", project: pn, agents: fixtureData.agents.filter((a) => a.project === pn).length, needs }; throw e; }
      }
      if (what === "hide") { setHidden({ until: body.until ?? null, archived: false }); return { ok: true }; }
      if (what === "unhide") { setHidden(null); return { ok: true }; }
      if (what === "close") return { ok: true, results: closeAll() };
      if (what === "archive") { const results = closeAll(); const archivedSessions = results.filter((r) => r.ok).map((r) => ({ name: r.name, identity: `id-${r.name}` })); setHidden({ until: null, archived: true, archivedSessions }); pr.archivedSessions = archivedSessions; return { ok: true, project: pn, results, archivedSessions }; }
      if (what === "restore") { setHidden(null); return { ok: true }; }
      if (what === "reopen-all") { const results = (pr.archivedSessions || []).map(({ name: n }) => { const a = fixtureApi.closed?.get(n); if (!a) return { name: n, ok: false, error: "no session" }; fixtureApi.closed.delete(n); fixtureData.agents.push({ ...a, state: "idle", needsHuman: false, hidden: false }); return { name: n, ok: true }; }); pr.archivedSessions = []; return { ok: true, results }; }
    }
    m = path.match(/^\/api\/agents\/([^/]+)\/paths(?:\?(.*))?$/);
    if (m && method === "GET") {
      const FS = ["web/", "web/app.js", "web/index.html", "web/style.css", "web/i18n.js", "web/vendor/", "web/vendor/katex.min.js", "src/", "src/main.rs", "src/paths.rs", "src/state.rs", "README.md", "Cargo.toml", "docs/", "docs/SPEC.md", "my notes/", "my notes/a b.md", "~/", "~/Sync/", "~/Sync/team@example.com/", "~/Sync/team@example.com/Shared Drive/", "~/Sync/team@example.com/Shared Drive/projects/", "~/Sync/team@example.com/Shared Drive/projects/2025/", "~/Sync/team@example.com/Shared Drive/projects/2025/protein-fold/", "~/Sync/team@example.com/Shared Drive/projects/2025/protein-fold/00_notes/", "~/Sync/team@example.com/Shared Drive/projects/2025/protein-fold/00_notes/retrain_plan.md", "~/Sync/team@example.com/Shared Drive/projects/2025/protein-fold/00_notes/band_a_only.md", "~/Sync/team@example.com/Shared Drive/projects/2025/protein-fold/00_notes/old/", "~/Sync/team@example.com/Shared Drive/projects/2025/protein-fold/00_notes/old/v0.44_summary.md", "~/Sync/team@example.com/Shared Drive/projects/2025/protein-fold/01_data/", "~/Sync/team@example.com/Shared Drive/projects/2025/protein-fold/README.md"];
      const q = new URLSearchParams(m[2] || "").get("q") || "";
      const dir = q.includes("/") ? q.slice(0, q.lastIndexOf("/") + 1) : "", rest = q.slice(dir.length).toLowerCase();
      const items = FS.filter((p) => p.startsWith(dir) && p !== dir && (rest ? p.slice(dir.length).toLowerCase().includes(rest) : !p.slice(dir.length).replace(/\/$/, "").includes("/")))
        .map((p) => ({ path: p, kind: p.endsWith("/") ? "dir" : "file", size: p.endsWith("/") ? null : 4096, mtime: Date.now() - 3600000, insert: /codex|agy/i.test(m[1]) ? p : / /.test(p) ? `@"${p}"` : `@${p}` }));
      return { items };
    }
    // Close / reopen: a closed agent leaves /api/state (history keeps it, so it shows as offline) and can be reopened.
    const closeOne = (n, b) => {
      const i = fixtureData.agents.findIndex((x) => x.name === n), a = fixtureData.agents[i];
      if (!a) return { name: n, ok: false, error: "unknown agent" };
      const k = kindOf(a);
      if ((k === "working" || k === "asking") && !b.confirmWorking) return { name: n, ok: false, reason: k };
      if (a.draft?.text && !b.discardDraft) return { name: n, ok: false, reason: "draft", draft: a.draft.text };
      (fixtureApi.closed ||= new Map()).set(n, a); fixtureData.agents.splice(i, 1);
      return { name: n, ok: true };
    };
    m = path.match(/^\/api\/agents\/([^/]+)\/(close|reopen)$/);
    if (m && method === "POST") {
      const n = decodeURIComponent(m[1]);
      if (m[2] === "reopen") { const a = fixtureApi.closed?.get(n); if (!a) { const e = new Error("no session to reopen"); e.status = 404; throw e; } fixtureApi.closed.delete(n); fixtureData.agents.push({ ...a, state: "idle", needsHuman: false }); return { ok: true }; }
      const r = closeOne(n, body);
      if (!r.ok) { const e = new Error(r.reason || r.error); e.status = r.reason ? 409 : 404; e.reason = r.reason; throw e; }
      return { ok: true };
    }
    if (method === "POST" && path === "/api/close") return { results: body.targets.map((n) => closeOne(n, body)) };
    m = path.match(/^\/api\/agents\/([^/]+)\/retry$/);
    if (m && method === "POST") { const a = fixtureData.agents.find((x) => x.name === decodeURIComponent(m[1])); return a ? retryOne(a) : null; }
    if (method === "POST" && path === "/api/retry-errors") return { results: fixtureData.agents.filter((a) => a.state === "error").map(retryOne) };
    m = path.match(/^\/api\/agents\/([^/]+)\/commands$/);
    if (m) {
      // Catalog per harness (omp / agy / others); templates come from meta via the UI fallback.
      const a = fixtureData.agents.find((x) => x.name === decodeURIComponent(m[1]));
      const sk = (name, description, scope = "user") => ({ kind: "skill", name, insert: `/${name}`, description, scope, source: "fixture" });
      const cmd = (name, description) => ({ kind: "command", name, insert: `/${name}`, description, scope: "builtin", source: "fixture" });
      const t = String(a?.agentType || "");
      const items = t === "omp" ? [sk("review-panel", "Three-role review (plan, critique, propose)"), sk("job-search", "Search and summarize job calls"), sk("research-report", "Markdown research report with traceable evidence"), sk("latex-section-workflow", "LaTeX section: draft → LaTeX", "project"), sk("commit-triage", "Commit and tidy git status"), cmd("model", "Switch model"), cmd("compact", "Summarize to free context"), cmd("clear", "New conversation")]
        : t === "antigravity" ? [sk("gemini-search", "Web search (Gemini grounding)", "builtin"), sk("retrain", "Model retraining checklist", "project"), cmd("plan", "Plan mode"), cmd("memory", "Show memory"), cmd("clear", "New conversation")]
        : [cmd("compact", "Summarize conversation"), cmd("clear", "New conversation")];
      if (t === "omp") items.push(sk("progress-report", "Team progress report (Markdown)"), sk("bibtex-gen", "BibTeX entries from arXiv, DOI or title"), sk("cv-review", "Write and review a CV"), sk("research-statement", "Research statement"), sk("project-status-report", "Project status report"), sk("config-snapshot", "Agent config snapshots"), sk("long-skill-name-for-truncation-check", "A long name to show truncation"));
      // Fixture stand-in for the harness's mid-message form (BRIEF supplies the real one).
      if (t === "omp") for (const it of items) if (it.kind === "skill") it.midInsert = `skill://${it.name}`;
      const prefixes = t === "omp" ? [{ prefix: "!", kind: "shell", warning: "! at the start runs a shell command in omp" }] : [];
      return { harness: t, items, prefixes, fetchedAt: fixtureData.generatedAt };
    }
    m = path.match(/^\/api\/agents\/([^/]+)\/answer$/);
    if (m) {
      // 202 {jobId}; the job advances one answer per poll. A text answer containing "실패" fails there.
      const a = fixtureData.agents.find((x) => x.name === decodeURIComponent(m[1]));
      const jobId = `J${Date.now().toString(36)}`;
      const n = body.answers?.length || 1, failAt = (body.answers || []).findIndex((x) => /실패/.test(x.text || ""));
      (fixtureApi.jobs ||= {})[jobId] = { polls: 0, n, failAt, a };
      return { jobId };
    }
    m = path.match(/^\/api\/answers\/([^/]+)$/);
    if (m) {
      const j = fixtureApi.jobs?.[m[1]];
      if (!j) { const e = new Error("unknown job"); e.status = 404; throw e; }
      const k = Math.floor(++j.polls / 2);
      if (j.failAt >= 0 && k >= j.failAt + 1) return { state: "failed", step: "typing", k: j.failAt, n: j.n, verified: false, reason: "입력창이 포커스를 잃었습니다 (질문 2)" };
      if (k < j.n) return { state: "running", step: k ? "selecting" : "focusing", k, n: j.n };
      if (j.a && j.a.ask) { j.a.ask = null; j.a.state = "working"; j.a.needsHuman = false; j.a.stateSince = fixtureData.generatedAt; setTimeout(refresh, 0); }
      return { state: "done", step: "verifying", k: j.n, n: j.n, verified: true };
    }
    return { ok: true };
  }

  // UI row keys → server agent paths: an offline same-name row's key is a display name; the server gets the real
  // name plus `identity=` so it resolves that ended session, not the live agent of the same name.
  function agentApiPath(path) {
    const m = /^\/api\/agents\/([^/?]+)(.*)$/.exec(path);
    if (!m || !model?.agents) return path;
    const key = decodeURIComponent(m[1]), id = idParam(key);
    if (!id && realNameOf(key) === key) return path;
    return `/api/agents/${enc(realNameOf(key))}${m[2]}${id ? `${m[2].includes("?") ? "&" : "?"}${id}` : ""}`;
  }
  async function api(method, path, body, { quiet = false } = {}) {
    path = agentApiPath(path);
    if (FIXTURE) return fixtureApi(method, path, body);
    // The server rejects state-changing requests without X-Adash (CSRF guard).
    const opt = { method, headers: method === "GET" ? {} : { "X-Adash": "1" } };
    if (body !== undefined) { opt.headers["Content-Type"] = "application/json"; opt.body = JSON.stringify(body); }
    const r = await fetch(path, opt);
    let j = null; try { j = await r.json(); } catch { /* empty body */ }
    if (!r.ok) {
      if (r.status === 403 && j?.error === "device_pending") showPairing(j);
      const m = (j && j.error) || (j && j.reason) || `HTTP ${r.status}`; if (!quiet) toast(`오류: ${m}`); const e = new Error(m); e.status = r.status; e.reason = j?.reason; e.data = j; throw e;
    }
    return j;
  }
  const act = (method, path, body, okMsg) => api(method, path, body).then((r) => { if (!FIXTURE) { toast(okMsg || "완료"); refresh(); } return r; }).catch(() => null);

  // ---------- data ----------
  // Demo / fixture data is recorded at a fixed moment; every epoch-ms timestamp is moved by the same offset so the
  // demo always reads as "now" (fixture.json generatedAt → page load).
  let fxShift = null;
  const fxMove = (x) => {
    if (Array.isArray(x)) return x.map(fxMove);
    if (x && typeof x === "object") { for (const k in x) x[k] = fxMove(x[k]); return x; }
    return typeof x === "number" && x > 1.6e12 && x < 2.2e12 ? x + fxShift : x;
  };
  async function fxJson(url) {
    if (fxShift == null) fxShift = Date.now() - (await (await fetch("fixture.json")).json()).generatedAt;
    return fxMove(await (await fetch(url)).json());
  }
  async function refresh() {
    try {
      if (FIXTURE) {
        if (!fixtureData) {
          fixtureData = await fxJson("fixture.json");
          fixtureTx = await fxJson("fixture_transcripts.json");
          fixtureLaunch = await fxJson("fixture_launch.json");
          // ?tmux=down: the adash tmux server does not answer (banner).
          if (new URLSearchParams(location.search).get("tmux") === "down") fixtureData.backend = { kind: "tmux", available: false };
          // ?net=partial|down: provider reachability (v26).
          const nq = new URLSearchParams(location.search).get("net"), t0 = Date.now();
          if (nq) fixtureData.network = { state: nq, since: t0 - 4 * 60000, providers: [
            { provider: "anthropic", state: nq === "down" ? "down" : "ok", latencyMs: nq === "down" ? null : 182, checkedAt: t0 - 9000, error: nq === "down" ? "connect timeout (5 s)" : null },
            { provider: "openai", state: nq === "down" ? "down" : "blocked", latencyMs: nq === "down" ? null : 240, checkedAt: t0 - 9000, error: nq === "down" ? "dns: no such host" : "403 unsupported_country" },
            { provider: "google", state: nq === "down" ? "down" : "ok", latencyMs: nq === "down" ? null : 95, checkedAt: t0 - 9000, error: null }] };
        }
        setState(fixtureData);
        // Demo only: flip an agent between working and done+unread to see the working → my-turn landing.
        window.__adashDemo ||= { flip: (name) => { const a = fixtureData.agents.find((x) => x.name === name); if (!a) return; const w = a.state === "working"; Object.assign(a, w ? { state: "done", needsHuman: true, unread: true, tool: null } : { state: "working", needsHuman: false, unread: false, tool: "bash" }); a.stateSince = now(); refresh(); } };
      } else {
        const r = await fetch("/api/state");
        if (r.status === 403) { const j = await r.json().catch(() => null); if (j?.error === "device_pending") return showPairing(j); }
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        hidePairing();
        setState(await r.json());
      }
    } catch (e) { $("#status").textContent = `연결 실패: ${e.message}`; connLost(e); }
  }
  // ---------- demo / recording filters (v21): ?project=A,B · ?mask=1 · ?demo=1 (= mask + hide capacity/reports/settings) ----------
  // Applied to the data as it arrives, so every view, count and the title badge come from the filtered set.
  const DEMO = (() => {
    const p = new URLSearchParams(location.search);
    const projects = p.get("project") ? new Set(p.get("project").split(",").map((x) => x.trim()).filter(Boolean)) : null;
    const demo = p.get("demo") === "1";
    return { projects, demo, mask: demo || p.get("mask") === "1" };
  })();
  document.body.classList.toggle("mask", DEMO.mask);
  document.body.classList.toggle("demo", DEMO.demo);
  // Hidden / archived projects (v32): /api/state hiddenProjects [{name, until, archived, agents, archivedSessions}] and
  // agents[].hidden. Hidden until `until` (null = until shown again); archived = hidden for good. Their agents leave
  // the board (the server already drops them from counts, notifications and push); the footer lists them.
  let hiddenProjects = [], HIDDEN = new Set(), rawAgentCount = 0;
  const inScope = (project) => (!DEMO.projects || DEMO.projects.has(project)) && !HIDDEN.has(project);
  function scopeState(s) {
    if (s) {
      hiddenProjects = (s.hiddenProjects || []).filter((p) => p.archived || p.until == null || p.until > Date.now()).map((p) => ({ name: p.name, until: p.until ?? null, archived: !!p.archived, archivedSessions: p.archivedSessions || [], agents: p.agents ?? (s.agents || []).filter((a) => a.project === p.name).length }));
      HIDDEN = new Set(hiddenProjects.map((p) => p.name));
      rawAgentCount = (s.agents || []).length;
      if ((s.agents || []).some((a) => a.hidden)) s = { ...s, agents: s.agents.filter((a) => !a.hidden) };
    }
    if ((!DEMO.projects && !HIDDEN.size) || !s) return s;
    const agents = (s.agents || []).filter((a) => inScope(a.project));
    const names = new Set(agents.map((a) => a.name));
    return { ...s, agents, counts: undefined,
      projects: (s.projects || []).filter((p) => inScope(p.name)),
      threads: (s.threads || []).filter((t) => names.has(t.from) || names.has(t.to)),
      magiRuns: (s.magiRuns || []).filter((r) => names.has(r.source)),
      humanTasks: (s.humanTasks || []).filter((t) => !t.project || inScope(t.project)) };
  }
  function scopeHistory(h) {
    if ((!DEMO.projects && !HIDDEN.size) || !h) return h;
    const agents = (h.agents || []).filter((a) => inScope(a.project));
    const names = new Set(agents.map((a) => a.name));
    return { ...h, agents, projects: (h.projects || []).filter((p) => inScope(p.name)), threads: (h.threads || []).filter((t) => names.has(t.from) || names.has(t.to)) };
  }
  // Restart-when-idle reservations (agent names); see the outdated section below.
  const odPending = new Set();
  function setState(s) {
    themeVersionSeen(s?.themeVersion);
    state = scopeState(s);
    // the server's restart-when-idle list is authoritative: a restarted or cancelled agent drops its ↻ at once
    if (Array.isArray(state?.restartPending)) { odPending.clear(); state.restartPending.forEach((n) => odPending.add(n)); }
    $("#status").textContent = `갱신 ${new Date(FIXTURE ? s.generatedAt : Date.now()).toLocaleTimeString("ko-KR")}`;
    connOk();
    if (!$("#dlg").open) render();
    highlightLaunched();
  }
  function startUpdates() {
    refresh().then(() => { refreshHistory(); refreshCapacity(false); });
    setInterval(() => refreshCapacity(false), 60000);
    setInterval(refreshHistory, 15000);
    window.addEventListener("resize", () => { if (state) render(); });
    setInterval(() => { if (ov.open && ov.name) { pollTranscript(); } }, 2000);
    setInterval(tickCountdowns, 1000);
    setInterval(renderWorkLine, 1000);
    if (FIXTURE) return;
    let poll = null;
    const startPoll = () => { if (!poll) poll = setInterval(refresh, 2000); };
    if (!window.EventSource) return startPoll();
    let es, got = false;
    try { es = new EventSource("/api/events"); } catch { return startPoll(); }
    es.addEventListener("state", (ev) => { got = true; try { setState(JSON.parse(ev.data)); } catch { /* ignore */ } });
    es.onerror = () => { es.close(); startPoll(); };
    setTimeout(() => { if (!got) { es.close(); startPoll(); } }, 5000);
  }

  // ---------- history (swimlanes) ----------
  const HOUR = 3600000;
  let hours = [6, 24, 72].includes(+localStorage.getItem("adash.hours")) ? +localStorage.getItem("adash.hours") : 24;
  let history = null, historyErr = null, fixtureHist = null;
  async function refreshHistory() {
    try {
      if (FIXTURE) {
        if (!fixtureHist) fixtureHist = await fxJson("fixture_history.json");
        history = scopeHistory(fixtureHistory(hours));
      } else history = scopeHistory(await api("GET", `/api/history?hours=${hours}`, undefined, { quiet: true }));
      historyErr = null;
    } catch (e) { history = null; historyErr = e.message; }
    if (state && !$("#dlg").open) render();
  }
  // Fixture stand-in for GET /api/history: clip the 72 h fixture to the window and derive project totals.
  function fixtureHistory(h) {
    const now = fixtureHist.now, from = now - h * HOUR;
    const agents = fixtureHist.agents.map((a) => ({ ...a, segments: a.segments.filter((s) => s.end > from).map((s) => ({ ...s, start: Math.max(s.start, from) })) })).filter((a) => a.segments.length);
    const projects = new Map();
    for (const a of agents) {
      const p = projects.get(a.project) || { name: a.project, score: fixtureData.projects.find((x) => x.name === a.project)?.score ?? 3, waitMinWindow: 0, waitingNow: 0, workingNow: 0 };
      for (const s of a.segments) if (s.state === "waiting") p.waitMinWindow += (s.end - s.start) / 60000;
      const last = a.segments[a.segments.length - 1];
      if (last.end >= now && last.state === "waiting") p.waitingNow++;
      if (last.end >= now && last.state === "working") p.workingNow++;
      projects.set(a.project, p);
    }
    return { now, from, agents, threads: fixtureHist.threads.filter((t) => (t.relayedAt || t.answeredAt || t.deliveredAt || t.createdAt) > from), projects: [...projects.values()] };
  }

  // Row model: history agents joined with live /api/state agents; falls back to state alone when history is unavailable.
  const KIND_ORDER = { asking: -1, error: -0.5, waiting: 0, working: 1, snoozed: 2, idle: 3, offline: 4 };
  let sortMode = localStorage.getItem("adash.sort") === "missed" ? "missed" : "recent";
  const expandedOffline = new Set(); // groups whose quiet offline agents are shown
  let model = { rows: [], groups: [], from: 0, now: 0, agents: [] };
  const lastSegEnd = (h) => h?.segments?.length ? h.segments[h.segments.length - 1].end : 0;
  const fmtStamp = (t) => { if (!t) return ""; const d = new Date(t), p = (n) => String(n).padStart(2, "0"); return `${p(d.getMonth() + 1)}.${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`; };
  // Row key → the agent's real name and session identity for API calls (offline same-name rows have a display key).
  const rowOf = (key) => model.agents.find((a) => a.name === key);
  const realNameOf = (key) => rowOf(key)?.realName || key;
  const idParam = (key) => { const r = rowOf(key); return r && !r.live && r.identity ? `identity=${encodeURIComponent(r.identity)}` : ""; };
  function buildModel() {
    const now = history?.now ?? Date.now();
    const from = history?.from ?? now - hours * HOUR;
    // Rows are keyed by session identity (history `identity` = state `paneKey`), not by name: a new live "WRITER" and
    // an ended old "WRITER" are two rows. A row's `name` is its UI key everywhere (data-agent, selection, ack, chat…),
    // so it must be unique: live agents keep their own name (the API resolves it to the live one); an offline
    // agent whose name is taken gets the backend's `displayName`, else "name · 종료 MM.DD HH:mm". `realName` and
    // `identity` stay for the transcript / reopen calls.
    const byKey = new Map();
    const hidSet = new Set((history?.agents || []).map((h) => h.identity).filter(Boolean));
    // state agents[].identity (requested from BRIEF) is exact; paneKey matched older history identities.
    const sid = (s) => (s.identity && hidSet.has(s.identity) ? s.identity : s.paneKey);
    const stateByPane = new Map((state.agents || []).filter((s) => sid(s) && hidSet.has(sid(s))).map((s) => [sid(s), s]));
    // State agents whose pane has no identity match (older backend / not yet in history) join by name: the live
    // history entry of that name first, else its most recent one.
    const byNameState = new Map((state.agents || []).filter((s) => !stateByPane.has(sid(s))).map((s) => [s.name, s]));
    const claimed = new Set();
    const taken = new Set((state.agents || []).map((s) => s.name));
    const hs = (history?.agents || []).slice().sort((p, q) => (q.live === true) - (p.live === true) || (lastSegEnd(q) - lastSegEnd(p)));
    for (const h of hs) {
      let live = (h.identity && stateByPane.get(h.identity)) || null;
      if (!live) { const s = byNameState.get(h.name); if (s && !claimed.has(s)) live = s; }
      if (live) { claimed.add(live); byKey.set(`s:${live.paneKey || live.name}`, { name: live.name, realName: live.name, identity: live.identity || h.identity || null, h, s: null }); continue; }
      let name = h.name;
      if (taken.has(name)) name = h.displayName || `${h.name} · 종료 ${fmtStamp(lastSegEnd(h))}`;
      for (let k = 2; taken.has(name); k++) name = `${h.displayName || h.name} (${k})`;
      taken.add(name);
      byKey.set(`h:${h.identity || h.name}`, { name, realName: h.name, identity: h.identity || null, h, s: null });
    }
    for (const s of state.agents || []) {
      const k = `s:${s.paneKey || s.name}`;
      const e = byKey.get(k) || { name: s.name, realName: s.name, identity: s.identity || s.paneKey || null, h: null, s: null };
      e.s = s; byKey.set(k, e);
    }
    const byName = byKey;
    const agents = [];
    const hasInteractions = (history?.agents || []).some((a) => Array.isArray(a.interactions));
    for (const e of byName.values()) {
      const s = e.s, h = e.h;
      const k0 = s ? kindOf(s) : "offline";
      // Snoozed (until a future time): every highlight is off: question, error, unread, waiting all read as
      // "snoozed"; `held` keeps what it would have been so the Now column can say it in gray. Working stays working.
      const snz = !!(s?.snoozedUntil && s.snoozedUntil > now);
      const kind = snz && k0 !== "working" ? "snoozed" : k0;
      let segs = h?.segments || [];
      if (!segs.length && s) segs = [{ state: kind, start: Math.max(from, s.stateSince || from), end: now, src: "recorded" }];
      // History may not know the open question yet: show it from askedAt on.
      if (kind === "asking" && segs.length && segs[segs.length - 1].state !== "asking") {
        const at = Math.max(from, s.ask?.askedAt || s.stateSince || now);
        segs = segs.filter((g) => g.start < at).map((g) => (g.end > at ? { ...g, end: at } : g)).concat([{ state: "asking", start: at, end: now, src: "recorded" }]);
      }
      const last = segs[segs.length - 1];
      const waitedMin = kind === "asking" ? Math.max(0, (now - (s.ask?.askedAt || s.stateSince || now)) / 60000) : kind === "waiting" ? (s ? waited(s) : (now - last.start) / 60000) : 0;
      const inter = (h?.interactions || []).slice().sort((p, q) => p.ts - q.ts);
      const humans = inter.filter((i) => i.kind === "human");
      const lastHuman = humans[humans.length - 1] || null;
      const lastHumanAt = h?.lastHumanAt ?? lastHuman?.ts ?? null;
      const waitingSince = s?.stateSince ?? last?.start ?? now;
      // Unread: the agent finished and is waiting on me, and that happened after my last message to it.
      // Without interaction data (older backend) every waiting agent counts as unread.
      // Backend `unread` (A2: lastAnswerAt > max(lastHumanAt, ackedAt)) wins; otherwise derive it from interactions.
      const unread0 = s && typeof s.unread === "boolean" ? s.unread : kind === "waiting" && (!hasInteractions || lastHumanAt == null || waitingSince > lastHumanAt);
      const unread = snz ? false : unread0;
      const held = kind === "snoozed" ? (k0 === "asking" ? "asking" : k0 === "error" ? "error" : unread0 ? "unread" : null) : null;
      // Timeline: the snoozed stretch draws as the hatch, not as a question / error / wait bar.
      if (kind === "snoozed" && segs.length) {
        const at = Math.max(from, Math.min(now, s.snoozedAt || segs[segs.length - 1].start));
        segs = segs.filter((g) => g.start < at).map((g) => (g.end > at ? { ...g, end: at } : g)).concat([{ state: "snoozed", start: at, end: now, src: "recorded" }]);
      }
      // Live = present in /api/state now; history's live flag lags behind a close from this dashboard.
      agents.push({ name: e.name, realName: e.realName, identity: e.identity, s, h, kind, held, snoozedUntil: snz ? s.snoozedUntil : null, segs, waitedMin, inter, lastHuman, lastHumanAt, unread, live: !!s, project: s?.project || h?.project || "?", role: s?.role ?? h?.role ?? null });
    }
    // Threads name their ends; with fromIdentity/toIdentity they attach to that session's row (an ended same-name
    // agent's row key differs from its name). Threads without identities (older ones) keep the name link.
    // The original names stay in _from/_to so rebuilding is idempotent.
    const keyById = new Map(agents.filter((a) => a.identity).map((a) => [a.identity, a.name]));
    for (const t of [...(history?.threads || []), ...(state.threads || [])]) {
      if (t._from === undefined) { t._from = t.from; t._to = t.to; }
      t.from = (t.fromIdentity && keyById.get(t.fromIdentity)) || t._from;
      t.to = (t.toIdentity && keyById.get(t.toIdentity)) || t._to;
    }
    const pinfo = new Map();
    for (const p of history?.projects || []) pinfo.set(p.name, { ...p });
    for (const a of agents) {
      if (!pinfo.has(a.project)) {
        const sp = state.projects?.find((x) => x.name === a.project);
        pinfo.set(a.project, { name: a.project, score: sp?.score ?? 3, waitMinWindow: sp?.humanWaitMin ?? 0, waitingNow: 0, workingNow: 0 });
      }
    }
    const show = agents.filter((a) => filter === "all" || (filter === "waiting" ? a.kind === "waiting" : a.live));
    // Ended (offline) agents, including an ended same-name "RS (종료됨 …)", fold into the group's "오프라인 N개
    // 숨김" row even if I talked to them in the window; they show only when expanded, after the live members.
    // The 라이브만 / 대기만 filters drop them outright (`show` above).
    const quietOffline = (a) => filter === "all" && !a.live;
    const groups = [];
    for (const [name, p] of pinfo) {
      const inGroup = show.filter((a) => a.project === name);
      const hidden = inGroup.filter(quietOffline);
      const members = inGroup.filter((a) => !quietOffline(a));
      if (!members.length && !hidden.length) continue;
      const byRecent = (x, y) => (y.lastHumanAt ?? -1) - (x.lastHumanAt ?? -1) || KIND_ORDER[x.kind] - KIND_ORDER[y.kind] || x.name.localeCompare(y.name);
      // "Missed first": unread replies, then other waiting agents, each oldest-contact first; the rest by recency.
      const missRank = (a) => (a.unread ? 0 : a.kind === "waiting" ? 1 : 2);
      const byMissed = (x, y) => missRank(x) - missRank(y) || (missRank(x) < 2 ? (x.lastHumanAt ?? -1) - (y.lastHumanAt ?? -1) : byRecent(x, y));
      // Open questions always lead their group, longest-open first.
      const askFirst = (x, y) => (["asking", "error"].includes(y.kind)) - (["asking", "error"].includes(x.kind)) || (x.kind === "asking" ? y.waitedMin - x.waitedMin : 0);
      members.sort((x, y) => askFirst(x, y) || (sortMode === "missed" ? byMissed : byRecent)(x, y));
      if (expandedOffline.has(name)) members.push(...hidden.slice().sort(byRecent));
      const sp = state.projects?.find((x) => x.name === name) || {};
      const all = agents.filter((a) => a.project === name);
      const unread = all.filter((a) => a.unread).length;
      const asking = all.filter((a) => a.kind === "asking").length;
      const score = p.score || sp.score || 1;
      groups.push({ name, p, sp, members, hidden, score, unread, asking, weight: (unread + 2 * asking) * score, lastHumanAt: Math.max(-1, ...all.map((a) => a.lastHumanAt ?? -1)), oldestMissed: Math.min(Infinity, ...all.filter((a) => a.unread).map((a) => a.lastHumanAt ?? 0)), waitingNow: all.filter((a) => a.kind === "waiting").length, cold: all.filter((a) => a.kind === "waiting" && cacheState(a.s) === "cold").length, allCount: all.length });
    }
    groups.sort(sortMode === "missed"
      ? (x, y) => y.weight - x.weight || x.oldestMissed - y.oldestMissed || x.name.localeCompare(y.name)
      : (x, y) => y.lastHumanAt - x.lastHumanAt || y.weight - x.weight || x.name.localeCompare(y.name));
    const rows = [];
    let y = 0;
    for (const g of groups) {
      g.y = y; rows.push({ type: "g", g, y, h: 28 }); y += 28;
      for (const a of g.members) { rows.push({ type: "a", a, g, y, h: 28 }); y += 28; }
      if (g.hidden.length) { rows.push({ type: "h", g, y, h: 24 }); y += 24; }
    }
    // 내 차례 = 질문 + 미확인 답변 + 기타 대기 (권한·승인 대기, 답변 보류, 입력창 초안으로 막힌 전달); snoozed excluded.
    const blockedByDraft = new Set((state.threads || []).filter((t) => t.status === "held_possible_draft").map((t) => t.to));
    for (const a of agents) {
      a.turn = !a.s || a.kind === "snoozed" ? null
        : a.kind === "asking" ? "asking"
        : a.kind === "error" ? "error"
        : a.unread ? "unread"
        : a.s.state === "waiting" || heldRelays(a.name).length || blockedByDraft.has(a.name) ? "other" : null;
    }
    const n = (k) => agents.filter((a) => a.turn === k).length;
    const local = { asking: n("asking"), error: n("error"), unread: n("unread"), other: n("other"), working: agents.filter((a) => a.s && a.kind === "working").length };
    local.mine = local.asking + local.error + local.unread + local.other;
    // The backend computes the counts once (`/api/state.counts`); the local rule is only the fallback.
    const c = state.counts && typeof state.counts.mine === "number" ? { ...local, ...state.counts } : local;
    const scored = (state.projects || []).some((p) => (p.impact != null && !(+p.impact === 3 && +p.leverage === 3 && +p.cost === 3)) || (p.score != null && Math.abs(+p.score - 3) > 1e-6));
    const recent = agents.flatMap((a) => a.inter.filter((i) => i.kind === "human").map((i) => ({ ...i, agent: a.name, a }))).sort((p, q) => q.ts - p.ts).slice(0, 20);
    const agentMsgs = agents.flatMap((a) => a.inter.filter((i) => i.kind === "agent" || i.kind === "relay").map((i) => ({ ...i, agent: a.name, a }))).sort((p, q) => q.ts - p.ts).slice(0, 40);
    trackMarks(agents);
    model = { rows, groups, from, now, agents, recent, agentMsgs, counts: c, scored, hasInteractions, height: y, totalWait: [...pinfo.values()].reduce((t, p) => t + (p.waitMinWindow || 0), 0) };
  }
  const agentRows = () => model.rows.filter((r) => r.type === "a");
  // Triage order over 내 차례: open questions, unread answers about to lose their cache, other unread, then 기타 대기.
  const triageOrder = () => {
    const r = agentRows().map((x) => x.a);
    return [...r.filter((a) => a.turn === "asking"), ...r.filter((a) => a.turn === "error"), ...r.filter((a) => a.turn === "unread" && soonCold(a.s)), ...r.filter((a) => a.turn === "unread" && !soonCold(a.s)), ...r.filter((a) => a.turn === "other")].map((a) => a.name);
  };
  const waitingOrder = triageOrder;
  const navOrder = () => { const t = triageOrder(); return [...t, ...agentRows().map((x) => x.a.name).filter((n) => !t.includes(n))]; };

  // ---------- network gate (v26) ----------
  // /api/state network: {state: ok|partial|down, providers: [{provider, state: ok|blocked|down, latencyMs, checkedAt, error}], since}
  const NET_KO = { ok: "정상", blocked: "차단됨 (지역 제한)", down: "접속 불가" };
  const PROV_LABEL = { anthropic: "Anthropic", openai: "OpenAI", google: "Google", zai: "z.ai" };
  const provKey = (p) => { p = String(p || "").toLowerCase(); return /anthropic|claude/.test(p) ? "anthropic" : /openai|codex|chatgpt/.test(p) ? "openai" : /google|gemini|antigravity/.test(p) ? "google" : /z\.?ai|glm|zhipu/.test(p) ? "zai" : p; };
  const provLabel = (p) => PROV_LABEL[provKey(p)] || p;
  // An agent's provider: the backend's `provider` when given, else from its model / harness.
  function providerOf(s) {
    if (!s) return null;
    if (s.provider || s.usage?.provider) return provKey(s.provider || s.usage.provider);
    const m = String(s.modelNorm || s.model || "").toLowerCase(), t = String(s.agentType || "").toLowerCase();
    if (/claude|opus|sonnet|haiku/.test(m) || t === "claude") return "anthropic";
    if (/gpt|codex|^o\d/.test(m) || t === "codex") return "openai";
    if (/gemini/.test(m) || t === "antigravity" || t === "agy") return "google";
    if (/glm/.test(m)) return "zai";
    return null;
  }
  // The not-ok provider entry this agent depends on, or null.
  function netOf(s) {
    const ps = state?.network?.providers;
    const k = ps && providerOf(s);
    const p = k && ps.find((x) => provKey(x.provider) === k);
    return p && p.state !== "ok" ? p : null;
  }
  const ICON_CLOUD_OFF = `<svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"><path d="M4.5 12.5h7a2.5 2.5 0 0 0 .6-4.9A4 4 0 0 0 5 6.2 3.2 3.2 0 0 0 4.5 12.5zM2 2l12 12" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
  const netIcon = (s) => { const p = netOf(s); return p ? `<span class="net-off ${p.state}" title="${esc(`${provLabel(p.provider)} ${NET_KO[p.state] || p.state} · 자동 전송은 네트워크 대기`)}">${ICON_CLOUD_OFF}</span>` : ""; };
  const netLine = (p) => `${provLabel(p.provider)}: ${NET_KO[p.state] || p.state}${p.latencyMs != null ? ` · ${p.latencyMs}ms` : ""}${p.checkedAt ? ` · 확인 ${hhmm(p.checkedAt)}` : ""}${p.error ? ` · ${String(p.error).slice(0, 80)}` : ""}`;
  function netSummary() {
    const n = state?.network;
    if (!n || n.state === "ok" || !n.state) return null;
    const bad = (n.providers || []).filter((p) => p.state !== "ok").map((p) => provLabel(p.provider));
    return { level: n.state === "down" ? "down" : "partial", text: n.state === "down" ? "네트워크 끊김 · VPN 확인" : `일부 제공자 접속 불가 (${bad.join("·") || "?"})`, tip: (n.providers || []).map(netLine).join("\n") + (n.since ? `\n${hhmm(n.since)}부터` : "") };
  }
  function renderNet() {
    const sum = netSummary(), pill = $("#net-pill"), ban = $("#net-banner");
    pill.hidden = !sum; ban.hidden = !sum;
    document.body.classList.toggle("net-bad", !!sum);
    if (!sum) return;
    pill.className = `net-pill ${sum.level}`; pill.dataset.act = "net-details";
    pill.innerHTML = `${ICON_CLOUD_OFF}<span>${esc(sum.text)}</span>`; pill.title = sum.tip;
    ban.className = `net-banner ${sum.level}`; ban.dataset.act = "net-details";
    ban.innerHTML = `${ICON_CLOUD_OFF}<b>${esc(sum.text)}</b><span>자동 전송은 복구될 때까지 대기 · 눌러서 자세히</span>`;
  }
  function netDetails() {
    const n = state?.network || {};
    const rows = (n.providers || []).map((p) => `<tr><td>${esc(provLabel(p.provider))}</td><td class="ns-${esc(p.state)}">${esc(NET_KO[p.state] || p.state)}</td><td>${p.latencyMs != null ? `${p.latencyMs}ms` : "–"}</td><td>${p.checkedAt ? hhmm(p.checkedAt) : "–"}</td><td class="muted small">${esc(String(p.error || ""))}</td></tr>`).join("");
    dialog(`<h3>네트워크 상태</h3><p class="muted small">${n.since ? `${new Date(n.since).toLocaleString("ko-KR")}부터 · ` : ""}접속할 수 없는 제공자의 에이전트에는 자동 전송(답변 전달·magi·이름 요청·자동 재시도)을 보내지 않고 대기합니다.</p><table class="net-tbl"><thead><tr><th>제공자</th><th>상태</th><th>지연</th><th>확인 시각</th><th>오류</th></tr></thead><tbody>${rows || `<tr><td colspan="5" class="muted">정보 없음</td></tr>`}</tbody></table>`, "지금 다시 확인", async () => {
      try { await api("POST", "/api/network/check", {}, { quiet: true }); toast("다시 확인했습니다", 2000); } catch (e) { toast(`확인 실패: ${e.message}`); }
      if (!FIXTURE) refresh(); else render();
    });
  }
  // Display settings: meta.autoRetryOnNetworkRecovery (server-wide, default true).
  async function loadNetRetry() {
    try { const m = await api("GET", "/api/meta", undefined, { quiet: true }); $("#fp-netretry").checked = m?.autoRetryOnNetworkRecovery !== false; } catch { /* keep default */ }
  }
  $("#fp-netretry").checked = true;
  $("#fp-netretry").addEventListener("change", async (e) => {
    const on = e.target.checked;
    try {
      const m = await api("GET", "/api/meta", undefined, { quiet: true });
      await api("PUT", "/api/meta", { ...m, autoRetryOnNetworkRecovery: on }, { quiet: true });
      toast(on ? "네트워크 복구 후 자동 재시도 켬" : "네트워크 복구 후 자동 재시도 끔", 2000);
    } catch (err) { e.target.checked = !on; toast(`저장 실패: ${err.message}`); }
  });
  setTimeout(loadNetRetry, 1500);
  // /api/state backend: {kind: "tmux", available}: while the adash tmux server does not answer, a banner with the
  // time since this page first saw it down (the server's watchdog restarts the tmux unit).
  let backendDownSince = null;
  // adash server unreachable (/api/state failed): separate from the tmux banner; always says when the last good
  // state arrived and that the board is no longer updating.
  let connLastOk = null, connDownSince = null;
  function connOk() { connLastOk = Date.now(); if (connDownSince != null) { connDownSince = null; renderConnBanner(); } }
  function connLost(e) { connDownSince ??= Date.now(); connErr = e?.message || ""; renderConnBanner(); }
  let connErr = "";
  function renderConnBanner() {
    const el = $("#conn-banner");
    if (!el) return;
    el.hidden = connDownSince == null;
    if (connDownSince == null) return;
    const last = connLastOk ? new Date(connLastOk).toLocaleTimeString("ko-KR") : null;
    el.innerHTML = `<span class="bb-icon" aria-hidden="true">◆</span><b>adash 서버에 연결할 수 없습니다</b><span>${last ? `마지막 수신 ${last} · 마지막으로 받은 상태를 표시하고 있으며 현재 갱신되지 않습니다` : "아직 상태를 받지 못했습니다. adash 서버 연결을 확인하세요"}${connErr ? ` (${esc(connErr)})` : ""}</span><span class="spacer"></span><button type="button" data-act="recheck">다시 연결</button>`;
  }
  setInterval(() => { if (connDownSince != null) renderConnBanner(); }, 15000);
  function renderBackendBanner() {
    const down = state?.backend?.available === false, el = $("#backend-banner");
    if (!down) backendDownSince = null; else backendDownSince ??= Date.now();
    el.hidden = !down;
    if (!down) return;
    const min = (Date.now() - backendDownSince) / 60000;
    el.innerHTML = `<span class="bb-icon" aria-hidden="true">◆</span><b>tmux 서버가 응답하지 않습니다</b><span>${fmtMin(min) || "1분 미만"} 경과 · 마지막 상태를 표시하며, 연결이 복구되기 전에는 보내기와 터미널 조작이 실패할 수 있습니다</span><span class="spacer"></span><button type="button" data-act="recheck">지금 다시 확인</button>`;
  }
  setInterval(renderBackendBanner, 15000);
  function render() {
    renderBackendBanner(); renderNet();
    const t0 = performance.now();
    buildModel();
    // Model cell width = longest label shown, so every badge in a column lines up.
    document.documentElement.style.setProperty("--hm-w", `${Math.max(3, ...(state?.agents || []).map((a) => modelFamily(a.usage?.model || a.model).length))}ch`);
    renderTop(); renderLanes(); renderRecent();
    if (isMobile()) renderMobile();
    if (ov.open) renderOverlay();
    render.ms = performance.now() - t0;
  }

  function renderTop() {
    const live = model.agents.filter((a) => a.s);
    const c = model.counts, sep = `<span class="sep"></span>`;
    const held = heldRelays().length;
    const cold = live.filter((a) => a.kind === "waiting" && cacheState(a.s) === "cold").length;
    const soonList = live.filter((a) => (a.turn === "unread" || a.turn === "asking") && soonCold(a.s)), soon = soonList.length;
    const soonTip = soonList.map((a) => `${a.name}: ${fmtMin((cacheExp(a.s) - nowTick()) / 60000) || "1분 미만"} 남음${cacheBasis(a.s) ? ` · ${cacheBasis(a.s)}` : ""}`).join("\n");
    const tip = `내 차례 = 질문 ${c.asking} + 오류 ${c.error || 0} + 미확인 답변 ${c.unread} + 기타 대기 ${c.other}\n오류 = 제공자 API 오류로 멈춘 에이전트\n기타 대기 = 권한·승인 대기, 답변 보류${held ? ` (${held})` : ""}, 입력창 초안으로 막힌 전달 · 미룬 것 제외\n누적 대기 ${(model.totalWait / 60).toFixed(1)}h (${hours}h) · 캐시 식음 ${cold}`;
    const nFix = (state?.agents || []).filter((a) => ["error", "auth"].includes(a.state) && !a.snoozedUntil).length;
    const retryAll = nFix >= 2 ? `<button class="retry-all" data-retry-all title="${esc(L2(`오류·로그인 문제 ${nFix}개를 한 번에 해결 (오류는 재시도, 로그인 필요는 다시 시작)`, `Fix ${nFix} errors and sign-in stops at once`))}"><span class="rl">${L2("문제 모두 해결", "Fix all")}</span><span class="rs">${L2("해결", "Fix")}</span></button>` : "";
    // Items carry a fold priority (data-f, higher folds first): when the top bar is short of room (English labels,
    // narrow windows) fitSummary() hides 작업 중 → 기타 → 곧 식음 → 미확인; the tooltip keeps every count.
    const si = (f, html, first) => `<span class="si"${f ? ` data-f="${f}"` : ""}>${first ? "" : sep}${html}</span>`;
    H("#summary").innerHTML = `<span title="${esc(tip)}">${si(0, `내 차례 <b class="${c.mine ? "alert" : ""}">${c.mine}</b>`, true)}${si(0, `<span class="lb">질문 </span><b class="${c.asking ? "ask-n" : ""}">${c.asking}</b>`)}${c.error ? si(0, `<span class="lb">오류 </span><b class="err-n">${c.error}</b>`) : ""}${si(1, `미확인 <b>${c.unread}</b>`)}${si(3, `기타 <b>${c.other}</b>`)}${soon ? si(2, `<span title="${esc(soonTip)}">곧 식음 <b class="held-n">${soon}</b></span>`) : ""}${si(4, `작업 중 <b>${c.working}</b>`)}</span>${retryAll}${odSummaryBtn()}` +
      "";
    // History unavailable: say so under the header instead of a hidden summary note.
    const hb = $("#hist-banner");
    if (hb) { hb.hidden = !historyErr; if (historyErr) H("#hist-banner").innerHTML = `<span>타임라인 기록을 불러오지 못해 현재 상태만 보여 줍니다</span><span class="muted small">${esc(historyErr)}</span><span class="spacer"></span><button type="button" data-act="recheck">기록 다시 불러오기</button>`; }
    $("#filter-sel").value = filter;
    $("#sort-sel").value = sortMode;
    $$("[data-hours]").forEach((b) => b.classList.toggle("on", +b.dataset.hours === hours));
    $$("#link-seg [data-links]").forEach((b) => b.classList.toggle("on", b.dataset.links === linkMode));
    document.title = c.mine ? `(${c.mine}) adash` : "adash";
  }

  // What happened after one of my messages: the agent's first stop (waiting/idle/offline segment) after it.
  function responseOf(i) {
    const a = i.a;
    const isLatest = a.lastHuman && a.lastHuman.ts === i.ts;
    const stop = a.segs.find((s) => s.start > i.ts && s.state !== "working" && s.state !== "snoozed");
    const nextHuman = a.inter.find((x) => x.kind === "human" && x.ts > i.ts);
    if (stop && (!nextHuman || stop.start < nextHuman.ts)) {
      if (isLatest && a.unread) return ["unread", "미확인 답변", `${ago(stop.start)} 답함`];
      return ["answered", "답변 있음", `${hhmm(stop.start)} 답함${nextHuman ? ` · 이후 ${hhmm(nextHuman.ts)}에 내가 답장` : ""}`];
    }
    if (nextHuman) return ["answered", "이어서 대화", `${hhmm(nextHuman.ts)}에 내가 다시 보냄`];
    if (a.kind === "working") return ["working", "작업 중", `${a.s?.tool ? `${a.s.tool} · ` : ""}${fmtMin((now() - i.ts) / 60000)}째`];
    if (a.kind === "waiting") return [a.unread ? "unread" : "answered", a.unread ? "미확인 답변" : "답변 있음", ""];
    return ["none", a.kind === "offline" ? "종료됨" : "응답 없음", ""];
  }

  let recentTab = localStorage.getItem("adash.recentTab") || "human";
  function renderRecent() {
    const items = model.recent, am = model.agentMsgs;
    const tabs = `<span class="rtabs"><button data-rtab="human" class="${recentTab === "human" ? "on" : ""}">최근 내 활동</button><button data-rtab="agent" class="${recentTab === "agent" ? "on" : ""}">에이전트 간${am.length ? ` <span class="muted">${am.length}</span>` : ""}</button></span>`;
    if (recentTab === "agent") {
      const head = `<h2>${tabs}<span class="muted">에이전트끼리 보낸 메시지 (검토·제안 요청, 회신, adash tell) · 클릭하면 받는 쪽 채팅 열기</span></h2>`;
      const KIND = { agent: "메시지", relay: "회신 전달" };
      H("#recent").innerHTML = head + (am.length ? `<div class="recent-list">${am.map((i) => {
        const text = String(i.text || "").trim();
        const first = text.split("\n").find((l) => l.trim()) || "";
        return `<div class="ri ai" data-agent-open="${esc(i.agent)}">
          <div class="when"><b>${ago(i.ts)}</b><span>${new Date(i.ts).toLocaleString("ko-KR", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit", hour12: false })}</span></div>
          <div><div class="who"><b>${esc(i.from || "?")}</b><span class="muted">→</span> <b>${esc(i.agent)}</b><span class="muted">· ${esc(i.a.project)}</span></div>
            <div class="said" title="${esc(text)}">${esc(first)}</div></div>
          <div class="resp"><span class="resp-tag">${esc(KIND[i.kind] || i.kind)}</span>${i.channel ? `<span class="muted">${esc(chanKo(i.channel))}</span>` : ""}</div></div>`;
      }).join("")}</div>` : `<p class="muted">${model.agents.some((a) => a.inter.some((i) => i.kind === "agent")) ? "이 기간에 에이전트 간 메시지 없음" : "에이전트 간 메시지 기록 없음 (백엔드가 human/agent 를 나눠 보내면 여기에 표시)"}</p>`);
      return;
    }
    const head = `<h2>${tabs}<span class="muted">내가 보낸 말 최근 ${items.length}개 · 클릭하면 채팅 열기</span></h2>`;
    if (!items.length) { H("#recent").innerHTML = head + `<p class="muted">${model.hasInteractions ? "이 기간에 보낸 말 없음" : "상호작용 기록 없음 (백엔드 대기)"}</p>`; return; }
    H("#recent").innerHTML = head + `<div class="recent-list">${items.map((i) => {
      const a = i.a, [cls, label, detail] = responseOf(i);
      const text = String(i.text || "").trim();
      return `<div class="ri" data-agent-open="${esc(i.agent)}">
        <div class="when"><b>${ago(i.ts)}</b><span>${new Date(i.ts).toLocaleString("ko-KR", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit", hour12: false })}</span></div>
        <div><div class="who"><b class="${a.s?.ambiguous ? "amb" : ""}">${esc(i.agent)}</b>${a.role ? `<span class="muted">· ${esc(a.role)}</span>` : ""}<span class="muted">· ${esc(a.project)}</span></div>
          ${askAnswersOf(i)?.length ? `<div class="said ask-ans" title="${esc(text)}"><span class="muted small">${t_("질문에 답함")}${esc(answeredVia(i))}</span>${answerSummaryHtml(askAnswersOf(i))}</div>` : `<div class="said" title="${esc(text)}">${i.btw || i.kind === "btw" ? '<span class="btw-tag" title="본 대화와 별개인 곁가지 질문 (omp btw)">btw</span> ' : ""}${i.skill ? `${skillTag(i.skill, i.agent)} ` : ""}${esc(text)}</div>`}</div>
        <div class="resp"><span class="resp-tag ${cls}">${label}</span>${detail ? `<span class="muted">${esc(detail)}</span>` : ""}</div></div>`;
    }).join("")}</div>`;
  }

  // ---------- clickable file paths in inline code ----------
  // Inline code that looks like a path is checked with POST /stat; only existing ones become links.
  const PATH_RE = /^(?:file:\/\/\/|~\/|\/|\.{1,2}\/)?[^\s(){}<>|;$`"'*?,]+$/;
  const looksLikePath = (t) => t.length >= 3 && t.length <= 400 && !/^[a-z][a-z0-9+.-]*:\/\/(?!\/)/i.test(t) && !/^https?:/i.test(t) && PATH_RE.test(t) && (t.includes("/") || /\.[A-Za-z0-9]{1,8}$/.test(t)) && !/^\d+(\.\d+)*$/.test(t);
  const stripLineSuffix = (t) => t.replace(/:(\d+)(-\d+)?$/, "");
  const statCache = new Map(); // `${agent}\0${path}` -> {exists, dir} | "pending"
  function normStat(r, paths) {
    const out = {};
    const arr = Array.isArray(r) ? r : Array.isArray(r?.results) ? r.results : Array.isArray(r?.paths) ? r.paths : null;
    if (arr) arr.forEach((x, i) => { const p = x?.path ?? paths[i]; out[p] = { exists: !!(x?.exists ?? x?.ok), dir: !!(x?.dir ?? x?.isDir ?? x?.kind === "dir") }; });
    else {
      const m = r?.results || r?.stat || r || {};
      for (const p of paths) { const x = m[p]; if (x != null) out[p] = typeof x === "boolean" ? { exists: x, dir: false } : { exists: !!(x.exists ?? true), dir: !!(x.dir ?? x.isDir ?? x.kind === "dir") }; }
    }
    return out;
  }
  function decoratePath(code, agent, p, info) {
    if (!info?.exists || code.closest("a")) return;
    const a = document.createElement("a");
    a.className = "pathlink"; a.href = "#";
    a.dataset.openPath = p; a.dataset.agent = agent;
    if (info.dir) a.dataset.dir = "1";
    a.title = `${info.dir ? "폴더 열기" : "열기"} · Alt+클릭: ${info.dir ? "열기" : "미리보기"}`;
    code.replaceWith(a); a.appendChild(code);
    a.insertAdjacentHTML("beforeend", `<span class="pi">${info.dir ? "▸" : "↗"}</span>`);
  }
  async function linkifyPaths(root, agent) {
    if (!agent) return;
    const todo = new Map(); // path -> [code elements]
    for (const code of root.querySelectorAll(".md code:not([data-pchk])")) {
      code.dataset.pchk = "1";
      if (code.closest("pre") || code.closest("a")) continue;
      const t = code.textContent.trim();
      if (!looksLikePath(t)) continue;
      const p = stripLineSuffix(t);
      const hit = statCache.get(`${agent}\0${p}`);
      if (hit && hit !== "pending") { decoratePath(code, agent, p, hit); continue; }
      if (!todo.has(p)) todo.set(p, []);
      todo.get(p).push(code);
    }
    if (!todo.size) return;
    const paths = [...todo.keys()].filter((p) => statCache.get(`${agent}\0${p}`) !== "pending");
    paths.forEach((p) => statCache.set(`${agent}\0${p}`, "pending"));
    let res = {};
    try { res = normStat(await api("POST", `/api/agents/${enc(agent)}/stat`, { paths }, { quiet: true }), paths); }
    catch { paths.forEach((p) => statCache.delete(`${agent}\0${p}`)); return; }
    for (const p of paths) statCache.set(`${agent}\0${p}`, res[p] || { exists: false, dir: false });
    for (const [p, codes] of todo) for (const c of codes) if (c.isConnected) decoratePath(c, agent, p, statCache.get(`${agent}\0${p}`));
  }

  // ---------- paths in plain prose (v16 addendum) ----------
  // `~/…`, `/…`, `file:///…`, and relative paths with an extension or a trailing `/`, also when a Korean particle
  // follows directly (`…report.md이고`, `…assets/에`). A relative path is tried first against the directory of the
  // last absolute path mentioned earlier in the same message, then as is; only paths that exist become links.
  const TXT_PATH_RE = /(?:file:\/\/\/|~\/|(?<![\w.~\/:-])\/(?=[\w.~가-힣]))[^\s<>()\[\]{}"'`*|;,]+|(?<![\w.~\/:-])(?:\.{1,2}\/)?[A-Za-z0-9_][\w.-]*(?:\/[\w.-]+)*(?:\/|\.[A-Za-z][A-Za-z0-9]{0,7}(?![A-Za-z0-9]))/g;
  const HANGUL = /[\u3131-\u318e\uac00-\ud7a3]/;
  // End the path at the last char of an extension or at "/" when Hangul follows; trim trailing punctuation.
  function cleanPath(raw) {
    for (let i = 1; i < raw.length; i++) {
      if (HANGUL.test(raw[i]) && !HANGUL.test(raw[i - 1])) {
        const head = raw.slice(0, i);
        if (/\.[A-Za-z][A-Za-z0-9]{0,7}$/.test(head) || head.endsWith("/")) { raw = head; break; }
      }
    }
    return raw.replace(/[.,:;!?)\]]+$/, "");
  }
  const isAbs = (p) => /^(?:file:\/\/\/|~\/|\/)/.test(p);
  const dirOf = (p) => (p.endsWith("/") ? p : p.slice(0, p.lastIndexOf("/") + 1));
  const SKIP_SEL = "code, pre, a, .katex, .pcand, .skchip, svg, .mmdblock, .svgblock, button";
  function textNodes(md) {
    const out = [], w = document.createTreeWalker(md, NodeFilter.SHOW_TEXT, { acceptNode: (n) => (n.parentElement?.closest(SKIP_SEL) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT) });
    while (w.nextNode()) out.push(w.currentNode);
    return out;
  }
  async function linkifyPlainPaths(root, agent) {
    if (!agent) return;
    const cands = [];
    for (const md of root.querySelectorAll(".md:not([data-plchk])")) {
      md.dataset.plchk = "1";
      let base = null, n = 0;
      // Absolute paths in inline code also set the base for later relative mentions.
      const order = [...md.querySelectorAll("code:not(pre code)")].map((c) => ({ c, t: c.textContent.trim() }));
      for (const node of textNodes(md)) {
        for (const o of order) if (o.c.compareDocumentPosition(node) & Node.DOCUMENT_POSITION_FOLLOWING && !o.used) { o.used = true; if (isAbs(o.t) && looksLikePath(o.t)) base = dirOf(stripLineSuffix(o.t)); }
        const text = node.nodeValue;
        TXT_PATH_RE.lastIndex = 0;
        const hits = [];
        for (let m; (m = TXT_PATH_RE.exec(text)) && n < 60;) {
          const p = cleanPath(m[0]);
          // "/word" or "/skill:name" alone is a slash command in prose, not a path.
          if (p.length < 3 || /^\d+(\.\d+)*\/?$/.test(p) || /^https?:/i.test(p) || /^\/[\w:.-]+$/.test(p) && !/\.[A-Za-z][A-Za-z0-9]{0,7}$/.test(p)) continue;
          const abs = isAbs(p);
          if (!abs && !p.includes("/") && !/\.[A-Za-z][A-Za-z0-9]{0,7}$/.test(p)) continue;
          const tries = abs ? [stripLineSuffix(p)] : [...(base ? [base + p.replace(/^\.\//, "")] : []), p];
          hits.push({ at: m.index, len: p.length, tries });
          if (abs) base = dirOf(stripLineSuffix(p));
          n++;
        }
        if (!hits.length) continue;
        // Split the text node into text + <span class="pcand"> placeholders (converted once /stat answers).
        const frag = document.createDocumentFragment();
        let last = 0;
        for (const h of hits) {
          frag.append(text.slice(last, h.at));
          const sp = document.createElement("span");
          sp.className = "pcand"; sp.textContent = text.slice(h.at, h.at + h.len);
          frag.append(sp); cands.push({ sp, tries: h.tries });
          last = h.at + h.len;
        }
        frag.append(text.slice(last));
        node.replaceWith(frag);
      }
    }
    if (!cands.length) return;
    const need = [...new Set(cands.flatMap((c) => c.tries))].filter((p) => !statCache.has(`${agent}\0${p}`));
    if (need.length) {
      need.forEach((p) => statCache.set(`${agent}\0${p}`, "pending"));
      let res = {};
      try { res = normStat(await api("POST", `/api/agents/${enc(agent)}/stat`, { paths: need }, { quiet: true }), need); }
      catch { need.forEach((p) => statCache.delete(`${agent}\0${p}`)); return; }
      for (const p of need) statCache.set(`${agent}\0${p}`, res[p] || { exists: false, dir: false });
    }
    for (const { sp, tries } of cands) {
      if (!sp.isConnected) continue;
      const p = tries.find((x) => statCache.get(`${agent}\0${x}`)?.exists);
      if (!p) { sp.replaceWith(sp.textContent); continue; }
      const info = statCache.get(`${agent}\0${p}`);
      const a = document.createElement("a");
      a.className = "pathlink plain"; a.href = "#"; a.dataset.openPath = p; a.dataset.agent = agent;
      if (info.dir) a.dataset.dir = "1";
      a.title = `${p}\n${info.dir ? "폴더 열기" : "열기"} · Alt+클릭: ${info.dir ? "열기" : "미리보기"}`;
      a.textContent = sp.textContent;
      sp.replaceWith(a);
    }
  }

  // ---------- skill / command names in messages (v16 addendum) ----------
  // Names that exist in THIS agent's cached catalog become chips (skills by bare name or /name, commands only as
  // /name so ordinary words like "clear" stay text). Click inserts `/<insert> ` into the composer; never sends.
  function highlightSkills(root, agent) {
    const items = cmdCache.get(agent)?.items?.filter((x) => (x.kind === "skill" && x.name.length >= 3) || x.kind === "command");
    if (!items?.length) return;
    const by = new Map();
    for (const it of items) { by.set(`/${it.name}`, it); if (it.insert && it.insert !== it.name) by.set(it.insert, it); if (it.kind === "skill") by.set(it.name, it); }
    const keys = [...by.keys()].sort((a, b) => b.length - a.length).map((k) => k.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&"));
    const re = new RegExp(`(?<![A-Za-z0-9_\\-/.:])(${keys.join("|")})(?![A-Za-z0-9_\\-])`, "g");
    const chipFor = (it, label) => {
      const sp = document.createElement("span");
      sp.className = `skchip k-${it.kind}`; sp.dataset.skInsert = it.insert || `/${it.name}`; if (it.midInsert) sp.dataset.skMid = it.midInsert;
      sp.title = `${it.kind === "skill" ? "스킬" : "명령"} · ${it.insert || `/${it.name}`}${it.scope ? ` · ${SCOPE_KO[it.scope] || it.scope}` : ""}\n${it.description || ""}\n누르면 입력창에 넣기`;
      sp.innerHTML = `${ICONS.skill}<span>${esc(label)}</span>`;
      return sp;
    };
    for (const md of root.querySelectorAll(".md:not([data-skchk])")) {
      md.dataset.skchk = "1";
      for (const c of md.querySelectorAll("code:not(pre code)")) {
        if (c.closest("a")) continue;
        const it = by.get(c.textContent.trim());
        if (it) c.replaceWith(chipFor(it, c.textContent.trim()));
      }
      for (const node of textNodes(md)) {
        const text = node.nodeValue;
        re.lastIndex = 0;
        if (!re.test(text)) continue;
        re.lastIndex = 0;
        const frag = document.createDocumentFragment();
        let last = 0;
        for (let m; (m = re.exec(text));) { frag.append(text.slice(last, m.index), chipFor(by.get(m[1]), m[1])); last = m.index + m[1].length; }
        frag.append(text.slice(last));
        node.replaceWith(frag);
      }
    }
  }
  // At a line start the plain insert form; mid-line the harness's midInsert (else insert), space-separated.
  function insertSkill(ins, mid) {
    const ta = $("#composer-text"), s = ta.selectionStart ?? ta.value.length, v = ta.value;
    const pre = v.slice(0, s), atStart = !pre || pre.endsWith("\n");
    const add = atStart ? `${ins} ` : `${/\s$/.test(pre) ? "" : " "}${mid || ins} `;
    ta.value = pre + add + v.slice(ta.selectionEnd ?? s);
    ta.focus(); ta.setSelectionRange(s + add.length, s + add.length);
  }
  // with: undefined (code → editor, else system app) | "editor" | "default" | "view" (dashboard viewer)
  // Right-click on a file link: how to open it.
  const openMenu = { el: null, link: null };
  function closeOpenMenu() { openMenu.el?.remove(); openMenu.el = null; }
  document.addEventListener("contextmenu", (e) => {
    const l = e.target.closest("a.pathlink[data-open-path]"); if (!l || l.dataset.dir || isMobile()) return;
    e.preventDefault(); closeOpenMenu();
    const m = document.createElement("div"); m.className = "pop open-menu";
    m.innerHTML = `<button data-open-with="editor">${esc(L2("편집기로 열기", "Open in editor"))}</button><button data-open-with="default">${esc(L2("기본 앱으로 열기", "Open with default app"))}</button><button data-open-with="view">${esc(L2("대시보드에서 보기", "View in dashboard"))}</button>`;
    (l.closest("#overlay") || document.body).appendChild(m); openMenu.el = m; openMenu.link = l; placePop(m, l);
  });
  document.addEventListener("pointerdown", (e) => { if (openMenu.el && !openMenu.el.contains(e.target)) closeOpenMenu(); }, true);
  async function openPath(link, alt, withApp) {
    const p = link.dataset.openPath, agent = link.dataset.agent;
    // phones view files here (nothing opens on the desktop from a phone)
    if (((alt || withApp === "view") || isMobile()) && !link.dataset.dir) {
      const a = document.createElement("a");
      a.dataset.local = p; a.dataset.agent = agent; a.setAttribute("href", fileUrl(agent, p, Date.now()));
      return openLocal(a);
    }
    try {
      const r = await api("POST", `/api/agents/${enc(agent)}/open`, { path: p, ...(withApp ? { with: withApp } : {}) });
      if (!FIXTURE) toast(`열기: ${p}${r?.app || r?.opener ? ` (${r.app || r.opener})` : ""}`, 2500);
    } catch { /* api() already toasted the error */ }
  }

  const hhmm = (t) => new Date(t).toLocaleTimeString("ko-KR", { hour: "2-digit", minute: "2-digit", hour12: false });
  const durTxt = (ms) => fmtMin(ms / 60000) || "0분";
  const ago = (t) => `${fmtMin(Math.max(0, (now() - t) / 60000)) || "0분"} 전`;
  const SEG_KO = { asking: "질문", working: "작업 중", waiting: "대기", snoozed: "미룸", idle: "유휴", offline: "오프라인", error: "오류" };
  // Snooze choices (v46): presets + a custom amount (hours/days, 1 min … 30 days); the server takes minutes.
  const SNZ_MIN = [60, 360, 720, 1440], SNZ_MAX = 43200;
  const snzLabel = (m) => (m % 1440 === 0 && m > 1440 ? L2(`${m / 1440}일`, `${m / 1440} d`) : m % 60 === 0 ? L2(`${m / 60}시간`, `${m / 60} h`) : L2(`${m}분`, `${m} min`));
  const snzCustomHtml = (act) => `<div class="snz-custom"><span class="muted small">${L2("직접", "Custom")}</span><input type="number" class="snz-n" min="0" step="any" inputmode="decimal" placeholder="3" aria-label="${esc(L2("미룰 양", "Amount"))}"><select class="snz-u" aria-label="${esc(L2("단위", "Unit"))}"><option value="60">${L2("시간", "hours")}</option><option value="1440">${L2("일", "days")}</option></select>${act ? `<button type="button" class="snz-go" data-act="${act}" disabled>${L2("적용", "Apply")}</button>` : ""}</div>`;
  const snzMenuHtml = (act) => SNZ_MIN.map((m) => `<button data-act="${act}" data-min="${m}"><b>${snzLabel(m)}</b></button>`).join("") + snzCustomHtml(act);
  const snzCustomMin = (box) => { const n = parseFloat(box.querySelector(".snz-n").value), m = Math.round(n * +box.querySelector(".snz-u").value); return Number.isFinite(m) && m >= 1 && m <= SNZ_MAX ? m : 0; };
  // "~14:00" today, "~11/7 14:00" on another day.
  function fmtUntil(t) {
    const d = new Date(t), hm = d.toLocaleTimeString(window.ADASH_LANG === "en" ? "en-GB" : "ko-KR", { hour: "2-digit", minute: "2-digit", hour12: false });
    return d.toDateString() === new Date().toDateString() ? hm : `${d.getMonth() + 1}/${d.getDate()} ${hm}`;
  }

  // Harness badge (item 5): a fixed-width two-part pill. The monogram + glyph shape carry the meaning
  // (omp ●, cc ■, cdx ▲, agy ◆, other ○), the tint is secondary; the model family is the second part.
  const HARNESS = { omp: "omp", claude: "cc", "claude-code": "cc", codex: "cdx", antigravity: "agy", agy: "agy", gemini: "gem" };
  // Short model label (≤6 chars): the model's own name, not the vendor ("gpt-6-astra" → "astra", "gemini-4-argon-medium" → "argon").
  function modelFamily(m) {
    m = String(m || "").toLowerCase().split("/").pop().replace(/:[a-z]+$/, "").replace(/-(low|medium|high|xhigh)$/, "");
    if (!m) return "";
    let x;
    if ((x = m.match(/^claude-(?:[\d.-]+-)?([a-z]+)/))) return x[1].slice(0, 6);
    if (/gpt-?oss/.test(m)) return "oss";
    if (/^codex/.test(m)) return "codex";
    if ((x = m.match(/^gpt-?[\d.]+(?:-([a-z]+))?/))) return x[1] && !/^(mini|nano|pro|turbo|codex|chat)$/.test(x[1]) ? x[1].slice(0, 6) : x[1] === "codex" ? "codex" : `gpt${m.match(/^gpt-?(\d+)/)?.[1] || ""}`.slice(0, 6);
    if ((x = m.match(/^(o\d)\b/))) return x[1];
    // gemini-<version>-<name>[-variant]: the named line ("argon", "carbon", "flash", "pro")
    if ((x = m.match(/^gemini-[\d.]+-([a-z]+)/))) return x[1].slice(0, 6);
    if (/gemini|argon|carbon/.test(m)) return (m.match(/argon|carbon|flash/) || ["gem"])[0];
    for (const [re, f] of [[/glm/, "glm"], [/grok/, "grok"], [/deepseek/, "ds"], [/qwen/, "qwen"], [/kimi/, "kimi"], [/opus|sonnet|haiku|fable|mythos/, null]]) if (re.test(m)) return f || m.match(re)[0];
    return (m.match(/[a-z]+/)?.[0] || "").slice(0, 6);
  }
  // Working mark (v44): the wordmark's speed lines drifting left. One shared keyframe; paused off-screen,
  // in hidden tabs and with reduced motion (then the lines stand still). Neutral colour only.
  // Both marks share the wordmark's speed lines (same two parallelograms as the logo SVG); state = class.
  // The wordmark's two speed lines (index.html, icons/favicon.svg and docs/media/logo.svg use the same points).
  const SPEED = { top: "7,7 22,7 18,11 3,11", bot: "6,14 16,14 13,17.5 3,17.5", box: "2.5 6.5 20 11.5" };
  const SL = (cls) => `<span class="sl ${cls}" aria-hidden="true"><svg viewBox="${SPEED.box}" width="16" height="9"><polygon points="${SPEED.top}"/><polygon class="sl2" points="${SPEED.bot}"/></svg></span>`;
  const WK = SL("wk");
  const WKa = (name) => (markAnim.get(name)?.kind === "lift" ? `<span class="wk-lift${markFx(name, "lift")}">${WK}</span>` : WK);
  // My-turn mark (v45, decision A): the same speed lines, stopped (two bars, turn colour) + how long it has waited.
  // Same slot and size as WK, so working → my turn reads as the lines stopping and changing colour. No motion.
  // Short units in every language (0m–59m, 1.0h–23.9h, 1d–99d) so the my-turn time slot stays narrow; the
  // spelled-out wait and the exact time are in the tooltip.
  const fmtWait = (m) => { m = Math.max(0, m || 0); return m < 60 ? `${Math.floor(m)}m` : m < 1440 ? `${(Math.floor(m / 6) / 10).toFixed(1)}h` : `${Math.min(99, Math.floor(m / 1440))}d`; };
  // "1시간 23분 · 14:05부터" / "1 h 23 min · since 14:05"
  const fmtWaitExact = (m, since) => {
    m = Math.max(0, Math.floor(m || 0)); const d = Math.floor(m / 1440), h = Math.floor((m % 1440) / 60), mm = m % 60, en = window.ADASH_LANG === "en";
    const parts = en ? [d && `${d} d`, h && `${h} h`, `${mm} min`] : [d && `${d}일`, h && `${h}시간`, `${mm}분`];
    const at = since ? new Date(since).toLocaleString(en ? "en-GB" : "ko-KR", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" }) : "";
    return parts.filter(Boolean).join(" ") + (at ? (en ? ` · since ${at}` : ` · ${at}부터`) : "");
  };
  const isMyTurn = (a) => a && (a.turn === "unread" || a.turn === "other");
  // Working → my turn, seen live on this screen: a one-off landing (≈450 ms). markTrack remembers each agent's mark
  // between renders; nothing plays on the first render, on filter changes or re-renders (same mark both times).
  const markPrev = new Map(), markAnim = new Map(); // name → "wk"|"tk"|"", name → {kind: "land"|"lift", at}
  let markSeeded = false;
  function trackMarks(agents) {
    const t = Date.now();
    for (const a of agents) {
      const m = a.kind === "working" ? "wk" : isMyTurn(a) ? "tk" : "";
      const p = markPrev.get(a.name);
      if (markSeeded && !document.hidden && p != null && p !== m && ((p === "wk" && m === "tk") || (p === "tk" && m === "wk"))) markAnim.set(a.name, { kind: m === "tk" ? "land" : "lift", at: t });
      markPrev.set(a.name, m);
    }
    markSeeded = true;
    for (const [n, x] of markAnim) if (t - x.at > 700) markAnim.delete(n);
  }
  // Class + negative delay so a re-render during the 450 ms continues the same animation instead of restarting it.
  const markFx = (name, kind) => { const x = markAnim.get(name); return x && x.kind === kind ? ` ${kind}" style="--fx-d:-${Math.min(700, Date.now() - x.at)}ms` : ""; };
  // My-turn mark (v45 rev): two longer logo lines (top 28px, bottom 20px, right-aligned, turn colour) at the right end
  // of the name column, the wait time to their left. Never covers the name: the name column ellipsizes first.
  const TKSVG = (cls) => `<svg class="tk-sl${cls}" viewBox="${SPEED.box}" width="28" height="16" aria-hidden="true"><polygon points="${SPEED.top}"/><polygon class="sl2" points="${SPEED.bot}"/></svg>`;
  // The landing starts where the working mark was (just after the name) and the lift where the my-turn mark was
  // (right end): measure once per render and hand the distance to CSS as --from-x.
  // The wait time sits in a fixed-width slot at the right end (width of the longest string fmtWait can produce in
  // this font, "23.9h"), left-aligned against the lines, so the lines never move as "3m" grows to "23.9h".
  // Re-measured on font/size/lang change.
  function measureTkSlot() {
    const host = $("#lane-labels") || document.body, probe = document.createElement("span");
    probe.className = "tk"; probe.style.cssText = "position:absolute;visibility:hidden;left:-9999px";
    // width:auto (not the slot being measured) and offsetWidth (CSS px, unaffected by the UI zoom)
    probe.innerHTML = `<span class="tk-t" style="width:auto"></span>`; host.appendChild(probe);
    const t = probe.firstChild; let w = 0;
    for (const m of [59, 1439, 99 * 1440]) { t.textContent = fmtWait(m); w = Math.max(w, t.offsetWidth); }
    probe.remove();
    if (w) { document.documentElement.style.setProperty("--tk-tw", `${Math.ceil(w) + 1}px`); measureTkSlot.lang = window.ADASH_LANG; }
  }
  document.fonts?.ready.then(measureTkSlot);
  function placeMarkFx(root) {
    if (!root || !markAnim.size) return;
    for (const el of root.querySelectorAll(".tk.land")) {
      const row = el.closest(".lrow, .mc-head, .ov-title"), nm = row?.querySelector(".nm, b, span"), svg = el.querySelector(".tk-slw");
      if (!nm || !svg) continue;
      const dx = Math.min(0, nm.getBoundingClientRect().right + 5 - svg.getBoundingClientRect().left);
      el.style.setProperty("--from-x", `${Math.round(dx)}px`);
      // the outgoing copy of the lines only lives for the landing
      el.querySelector(".tk-sl-n")?.addEventListener("animationend", (e) => e.currentTarget.remove(), { once: true });
    }
    for (const el of root.querySelectorAll(".wk-lift")) {
      const row = el.closest(".lrow"); if (!row) continue;
      const dx = Math.max(0, row.getBoundingClientRect().right - 40 - el.getBoundingClientRect().left);
      el.style.setProperty("--from-x", `${Math.round(dx)}px`);
    }
  }
  const TK = (a) => `<span class="tk${markFx(a.name, "land")}" title="${esc(L2(`내 차례 · ${fmtWaitExact(a.waitedMin, a.s?.stateSince ?? a.stateSince)} 기다림`, `Your turn · waiting ${fmtWaitExact(a.waitedMin, a.s?.stateSince ?? a.stateSince)}`))}"><span class="tk-slw">${TKSVG("")}${markAnim.get(a.name)?.kind === "land" ? TKSVG(" tk-sl-n") : ""}</span><span class="tk-t">${esc(fmtWait(a.waitedMin))}</span></span>`;
  function harnessBadge(s, live = true) {
    if (!s) return `<span class="hb h-none" title="종료됨"><i class="hg"></i><span class="hn">·</span></span>`;
    const t = String(s.agentType || "").toLowerCase(), mono = HARNESS[t] || (t.slice(0, 3) || "?"), cls = HARNESS[t] ? mono : "other";
    // usage.model is the live model (it follows /model switches); s.model is what the session was started with.
    const mdl = s.usage?.model || s.model, fam = modelFamily(mdl);
    return `<span class="hb h-${cls}${fam ? " has-m" : ""}${live ? "" : " off"}" title="${esc(`${s.agentType || "?"} · ${mdl || L2("모델 알 수 없음", "model unknown")}${s.role ? `\n역할: ${s.role}` : ""}${live ? "" : "\n종료됨"}`)}"><i class="hg"></i><span class="hn">${esc(mono)}</span>${fam ? `<span class="hm">${esc(fam)}</span>` : ""}</span>${live ? odMark(s) : ""}`;
  }
  // ---------- harness updated under a running session (v46): /api/state agents[].outdated {installed, running, why} ----------
  // Restart-when-idle reservations: /api/state restartPending (synced in setState), and the
  // /api/restart-outdated response `pending` right after a click.
  // installedAt (binary mtime) / startedAt (process start), ms: shown when the server sends them.
  const odHm = (t) => { const d = new Date(+t); if (!t || Number.isNaN(+d)) return ""; const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`; return d.toDateString() === new Date().toDateString() ? hm : `${d.getMonth() + 1}/${d.getDate()} ${hm}`; };
  const odTip = (o) => [L2(`실행 중 ${o.running || "?"} → 설치 ${o.installed || "?"}`, `Running ${o.running || "?"} → installed ${o.installed || "?"}`), o.installedAt ? L2(`업데이트 ${odHm(o.installedAt)}`, `updated ${odHm(o.installedAt)}`) : "", o.startedAt ? L2(`이 세션 시작 ${odHm(o.startedAt)}`, `session started ${odHm(o.startedAt)}`) : "", L2("다시 시작하면 적용", "restart to apply")].filter(Boolean).join(" · ");
  function odMark(s) {
    if (odPending.has(s.name)) return `<span class="od-mark pend" title="${esc(L2("작업이 끝나면 다시 시작 예약됨 · 누르면 취소", "Restart when idle is scheduled · click to cancel"))}" data-od-cancel="${esc(s.name)}">↻</span>`;
    return s.outdated ? `<span class="od-mark" title="${esc(odTip(s.outdated))}">↑</span>` : "";
  }
  const odAgents = () => (state?.agents || []).filter((a) => a.outdated);
  // Mirrors outdated::plan(): what a bulk restart will do with each agent.
  function odPlan(a, whenIdle, nowAsking) {
    if (a.state === "trust") return "skip_trust";
    if (a.state === "asking") return nowAsking ? "now" : whenIdle ? "later" : "skip_asking";
    if (String(a.draft?.text || "").trim()) return "skip_draft";
    if (a.state === "working") return whenIdle ? "later" : "skip_working";
    return "now";
  }
  const OD_ACT = { now: ["바로 재시작", "restart now"], later: ["작업 끝나면 재시작", "restart when idle"], skip_asking: ["건너뜀: 질문 중", "skip: asking"], skip_trust: ["건너뜀: 폴더 신뢰 필요 (카드에서 '이 폴더 신뢰')", "skip: folder trust needed"], skip_draft: ["건너뜀: 입력창 초안 있음", "skip: unsent draft"], skip_working: ["건너뜀: 작업 중", "skip: working"], not_outdated: ["건너뜀: 이미 최신", "skip: up to date"], sign_in_first: ["건너뜀: 먼저 로그인 필요", "skip: sign in first"] };
  const odActLabel = (k) => { const v = OD_ACT[k]; return v ? L2(v[0], v[1]) : k; };
  function odSummaryBtn() {
    const list = odAgents(); if (!list.length) return "";
    const by = {}; list.forEach((a) => (by[a.agentType || "?"] = (by[a.agentType || "?"] || 0) + 1));
    const tip = Object.entries(by).map(([k, n]) => `${k} ${n}`).join(" · ");
    return `<button type="button" class="od-btn" data-act="od-all" title="${esc(`${L2("하네스가 업데이트된 세션", "Sessions whose harness was updated")}\n${tip}`)}">↑ ${L2(`업데이트된 세션 ${list.length} · 모두 재시작`, `${list.length} updated · restart all`)}</button>`;
  }
  function odDialog(names) {
    const list = odAgents().filter((a) => !names || names.includes(a.name));
    if (!list.length) return toast(L2("업데이트된 세션이 없습니다", "No updated sessions"));
    const rows = (wi, na) => list.map((a) => { const p = odPlan(a, wi, na); const lab = p === "later" && a.state === "asking" ? L2("질문 답하면 재시작", "restart after the answer") : odActLabel(p); return `<tr><td>${esc(a.name)}</td><td class="muted">${esc(a.agentType || "")} ${esc(a.outdated.running || "")}→${esc(a.outdated.installed || "")}</td><td class="od-act-${p}">${esc(lab)}</td></tr>`; }).join("");
    dialog(`<h3>${esc(L2(`다시 시작 · ${list.length}개`, `Restart · ${list.length}`))}</h3><p class="muted small">${esc(L2("다시 시작하면 업데이트된 하네스로 같은 세션을 다시 엽니다. 질문 중이거나 입력창 초안이 있는 세션은 건너뜁니다.", "Each session reopens on the updated harness. Sessions asking a question or holding a draft are skipped."))}</p>
      <label class="ld-check"><input type="checkbox" id="od-wi" checked> ${esc(L2("작업 중인 세션도 끝나면 재시작", "Also restart working sessions when they finish"))}</label>
      ${list.some((a) => a.state === "asking") ? `<label class="ld-check"><input type="checkbox" id="od-na"> ${esc(L2("질문 중인 세션도 지금 재시작 (질문은 취소되고, 다시 물어보라고 알림)", "Restart sessions with an open question now (the question is cancelled; the agent is told to ask again)"))}</label>` : ""}
      <div class="od-list"><table id="od-tbl">${rows(true, false)}</table></div>`, L2("다시 시작", "Restart"), () => odRun(list.map((a) => a.name), $("#od-wi")?.checked !== false, !!$("#od-na")?.checked));
    const upd = () => { $("#od-tbl").innerHTML = rows($("#od-wi").checked, !!$("#od-na")?.checked); };
    $("#od-wi").addEventListener("change", upd); $("#od-na")?.addEventListener("change", upd);
  }
  async function odRun(names, whenIdle, restartAsking) {
    let r;
    try { r = await api("POST", "/api/restart-outdated", { names, whenIdle, restartAsking: !!restartAsking }); } catch (e) { return toast(L2(`다시 시작 실패: ${e.message}`, `Restart failed: ${e.message}`)); }
    odPending.clear(); (r?.pending || []).forEach((n) => odPending.add(n));
    const res = r?.results || [], n = (f) => res.filter(f).length;
    const now = n((x) => x.action !== "later" && x.action !== "skip"), later = n((x) => x.action === "later"), skip = n((x) => x.action === "skip");
    toast(L2(`재시작 ${now} · 예약 ${later} · 건너뜀 ${skip}`, `restarted ${now} · scheduled ${later} · skipped ${skip}`), 4000);
    if (!FIXTURE) refresh(); else render();
  }
  async function odCancel(name) {
    try { const r = await api("POST", "/api/restart-outdated", { cancel: true, names: [name] }); odPending.clear(); (r?.pending || []).forEach((n) => odPending.add(n)); }
    catch (e) { return toast(e.message); }
    toast(L2(`${name} 재시작 예약 취소`, `${name} restart cancelled`), 2000); render();
  }
  // 연결 보기 (item 2): basic = request spans + connector ticks, all = every curve, off = nothing.
  let linkMode = localStorage.getItem("adash.links") || "basic";
  let linkHover = { name: null, thr: null };
  function setLinkMode(m) {
    linkMode = m; localStorage.setItem("adash.links", m);
    document.body.dataset.links = m;
    $$("#link-seg [data-links]").forEach((b) => b.classList.toggle("on", b.dataset.links === m));
    renderLanes();
  }
  document.body.dataset.links = linkMode;
  // Light up one agent's connections (and both rows), or one thread's.
  function litLinks(name, thr, keepRows) {
    const tl = $("#timeline");
    tl.querySelectorAll(".lit").forEach((el) => el.classList.remove("lit"));
    const rows = new Set();
    if (thr) tl.querySelectorAll(`[data-thr="${CSS.escape(thr)}"]`).forEach((el) => { el.classList.add("lit"); rows.add(el.dataset.a); rows.add(el.dataset.b); });
    else if (name) tl.querySelectorAll(`[data-a="${CSS.escape(name)}"], [data-b="${CSS.escape(name)}"]`).forEach((el) => { el.classList.add("lit"); rows.add(el.dataset.a); rows.add(el.dataset.b); });
    if (!keepRows) $$(".lrow").forEach((el) => el.classList.toggle("peer", rows.has(el.dataset.agent) && el.dataset.agent !== name));
  }
  // 10 px stroke icons for the status column chips (currentColor = the chip's color).
  const svgI = (d) => `<svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">${d}</svg>`;
  const ICONS = {
    err: svgI(`<circle cx="5" cy="5" r="4.2" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M5 2.6v3M5 7.2v.2" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>`),
    skill: svgI(`<path d="M5 .9l1.2 2.5 2.7.4-2 1.9.5 2.7L5 7.1 2.6 8.4l.5-2.7-2-1.9 2.7-.4z" fill="none" stroke="currentColor" stroke-width="1" stroke-linejoin="round"/>`),
    dec: svgI(`<path d="M1.2 2.4l1 1 1.6-1.8M1.2 6.6l1 1 1.6-1.8" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/><path d="M5.4 2.6h3.6M5.4 6.8h3.6" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>`),
    held: svgI(`<rect x="2" y="1.6" width="2" height="6.8" rx=".6" fill="currentColor"/><rect x="6" y="1.6" width="2" height="6.8" rx=".6" fill="currentColor"/>`),
    draft: svgI(`<path d="M1.6 8.4l.5-2.1 5-5 1.6 1.6-5 5z" fill="none" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"/>`),
    magi: svgI(`<circle cx="2" cy="5" r="1.3" fill="currentColor"/><path d="M3.2 5h1.6M4.8 2.2v5.6M4.8 2.2h2.4M4.8 5h2.4M4.8 7.8h2.4" fill="none" stroke="currentColor" stroke-width="1.1"/><circle cx="8.3" cy="2.2" r=".9" fill="currentColor"/><circle cx="8.3" cy="5" r=".9" fill="currentColor"/><circle cx="8.3" cy="7.8" r=".9" fill="currentColor"/>`),
    retry: svgI(`<path d="M8.3 3.2A3.8 3.8 0 1 0 8.8 6" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/><path d="M8.6 1v2.6H6" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>`),
  };
  // Fold the lowest-priority status items (largest data-p) into a "+k" chip until the row fits.
  function fitSummary() {
    const el = $("#summary"), items = [...el.querySelectorAll(".si[data-f]")];
    // Overflow = the last visible piece ends past the box (scrollWidth alone misses text cut by the ellipsis).
    const over = () => { const right = el.getBoundingClientRect().right; return [...el.querySelectorAll(".si:not([hidden]), .retry-all")].some((x) => x.getBoundingClientRect().right > right + 0.5) || el.scrollWidth > el.clientWidth + 0.5; };
    items.forEach((x) => { x.hidden = false; });
    const rb = el.querySelector(".retry-all");
    if (rb) rb.classList.remove("short");
    el.classList.remove("tight");
    for (const x of items.sort((p, q) => +q.dataset.f - +p.dataset.f)) { if (!over()) break; x.hidden = true; }
    // Still short of room: the retry button keeps only its verb.
    if (rb && over()) rb.classList.add("short");
    // Last resort: 질문 / 오류 keep only their coloured counts (labels stay in the tooltip).
    if (over()) el.classList.add("tight");
  }
  // Re-measure after the i18n layer has swapped in the (longer) English text, which happens after this render.
  const refit = () => requestAnimationFrame(() => { fitInfoRows(); fitSummary(); });
  // The summary's room changes when the capacity readout or the network pill loads after render.
  { const ro = new ResizeObserver(() => requestAnimationFrame(fitSummary)); ro.observe($(".top")); ro.observe($("#summary")); ro.observe($("#capacity")); }
  window.addEventListener("resize", refit);
  // Now column (v40.2). 1) Measure every item at its natural width (".measure": nothing shrinks) and fold the
  // optional marks, highest data-p first (magi → progress → cache → draft → held → decisions), until the row fits.
  // 2) Release: only the auxiliary text (.st-a: elapsed time, tool name) may shrink with "…"; the core state, the
  // error code and ↻ keep their width (the status container's min-width is pinned to its core part).
  function fitInfoRows() {
    for (const row of document.querySelectorAll("#lane-info .lrow[data-agent]")) {
      const items = [...row.querySelectorAll("[data-p]")], more = row.querySelector(".c-more"), st = row.querySelector(".st-txt");
      if (st) st.style.minWidth = "";
      row.classList.add("measure");
      items.forEach((el) => { el.hidden = false; });
      more.hidden = true;
      const fits = () => row.scrollWidth <= row.clientWidth + 0.5;
      if (!fits()) {
        const order = items.filter((el) => +el.dataset.p > 0).sort((x, y) => +y.dataset.p - +x.dataset.p);
        let n = 0;
        for (const el of order) {
          el.hidden = true; n++;
          more.hidden = false; more.textContent = `+${n}`;
          if (fits()) break;
        }
        more.dataset.tip = items.map((el) => el.dataset.tip).filter(Boolean).join("\n\n");
      }
      if (st) { const core = st.querySelector(".st-c"); const pad = st.offsetWidth - [...st.children].reduce((w, c) => w + c.offsetWidth, 0); st.style.minWidth = core ? `${Math.ceil(core.offsetWidth + Math.max(0, pad))}px` : "0px"; }
      row.classList.remove("measure");
    }
  }
  window.addEventListener("resize", () => requestAnimationFrame(fitInfoRows));
  function renderLanes() {
    const { rows, from, now } = model;
    const W = Math.max(50, $("#lane-time").clientWidth);
    const X = (t) => ((t - from) / (now - from)) * W;
    const step = hours <= 6 ? 1 : hours <= 24 ? 2 : 6;
    // Axis + grid
    const ticks = [];
    const first = Math.ceil(from / HOUR) * HOUR;
    for (let t = first; t <= now; t += HOUR) {
      const d = new Date(t);
      if (d.getHours() % step) continue;
      if (X(t) > W - 34) continue;
      ticks.push({ x: X(t), label: d.getHours() === 0 ? `${d.getMonth() + 1}/${d.getDate()}` : `${d.getHours()}시` });
    }
    H("#axis").innerHTML = ticks.map((k) => `<line x1="${k.x.toFixed(1)}" x2="${k.x.toFixed(1)}" y1="14" y2="22" stroke="var(--border)"></line><text x="${(k.x + 2).toFixed(1)}" y="12">${k.label}</text>`).join("") +
      `<text class="now" x="${W - 2}" y="12" text-anchor="end">지금</text>`;
    const svg = [];
    svg.push(`<defs><pattern id="err-stripe" width="5" height="5" patternUnits="userSpaceOnUse" patternTransform="rotate(-45)"><rect width="5" height="5" fill="var(--err-bg)"></rect><line x1="0" y1="0" x2="0" y2="5" stroke="var(--cold)" stroke-width="2.4"></line></pattern><pattern id="hatch" width="5" height="5" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><line x1="0" y1="0" x2="0" y2="5" stroke="var(--timeline-snoozed)" stroke-width="1.2" opacity=".6"></line></pattern>
      <marker id="arr" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path class="arr" d="M0,0 L8,4 L0,8 z"></path></marker></defs>`);
    for (const r of rows) {
      if (r.type === "g") svg.push(`<line class="gsep" x1="0" x2="${W}" y1="${r.y + 0.5}" y2="${r.y + 0.5}"></line>`);
      else if (r.type === "a" && r.a.name === selName) svg.push(`<rect class="rowsel" x="0" y="${r.y}" width="${W}" height="${r.h}"></rect>`);
    }
    for (const k of ticks) svg.push(`<line class="grid" x1="${k.x.toFixed(1)}" x2="${k.x.toFixed(1)}" y1="0" y2="${model.height}"></line>`);
    const rowY = new Map(), rowTop = new Map();
    for (const r of rows) if (r.type === "a") { rowY.set(r.a.name, r.y + r.h / 2); rowTop.set(r.a.name, r.y); }
    // Bars: working/asking saturated; only the CURRENT waiting span (last answer → now) carries the waiting tint,
    // earlier waiting stretches are nearly transparent so the column stays readable.
    for (const r of rows) {
      if (r.type !== "a") continue;
      const segs = r.a.segs, lastSeg = segs[segs.length - 1];
      for (const s of segs) {
        if (s.state === "offline" || s.end <= from) continue;
        const tip = `${raw(r.a.name)} · ${SEG_KO[s.state] || s.state}\n${hhmm(s.start)}–${s.end >= now ? "지금" : hhmm(s.end)} · ${durTxt(s.end - s.start)}${s.src === "reconstructed" ? " · 복원" : ""}`;
        const rec = s.src === "reconstructed" ? " rec" : "";
        // The turn-coloured wait bar only while it really is my turn (unread answer / other wait, not snoozed); a waiting
        // stretch after I have read the answer is idle time and draws as the gray hairline.
        const cls = s.state === "waiting" ? (s === lastSeg && s.end >= now - 60000 && (r.a.turn === "unread" || r.a.turn === "other") ? "s-wait" : "s-wait-old") : `s-${s.state}`;
        const x0 = X(Math.max(s.start, from)), x1 = X(s.end), w = Math.max(1, x1 - x0);
        // Thickness carries the meaning (readable in grayscale and at README size): needs-me 10 / 8 px, working
        // 5.5 px steel, snoozed 4 px hatch, idle and earlier waiting a 2 px hairline.
        const [dy, h] = cls === "s-asking" || cls === "s-error" ? [9, 10] : cls === "s-wait" ? [10, 8] : cls === "s-working" ? [11.25, 5.5] : cls === "s-snoozed" ? [12, 4] : [13, 2];
        svg.push(`<rect class="seg-r ${cls}${rec}" x="${x0.toFixed(1)}" y="${r.y + dy}" width="${w.toFixed(1)}" height="${h}" rx="${Math.min(2.5, h / 2)}" data-tip="${esc(tip)}"></rect>`);
        const live = s === lastSeg && s.end >= now - 60000;
        if (live && cls === "s-asking" && w > 8) svg.push(`<circle class="s-core" cx="${(x1 - 5).toFixed(1)}" cy="${r.y + 14}" r="2"></circle>`);
        if (live && cls === "s-error") svg.push(`<path class="s-diamond" d="M${x1.toFixed(1)},${r.y + 8.5} l5.5,5.5 l-5.5,5.5 l-5.5,-5.5 z"></path>`);
      }
    }
    // Review/propose requests (item 2): a thin band on the TARGET row from delivery to answer, a requester-initial
    // chip at its start, a return tick on the requester row, and straight 1 px connector ticks between the rows.
    // Full curves only on hover/selection ("모두" shows them all). magi runs: one fan-out bracket per run.
    const curves = [];
    if (linkMode !== "off") {
      const thrKind = (t) => ((t.magiKind || t.kind || t.mode) === "propose" ? "propose" : "review");
      const OPEN = new Set(["queued", "delivered", "delivery_unconfirmed", "held_possible_draft"]);
      const runs = new Map();
      for (const t of model.agents.length ? history?.threads || [] : []) {
        const ya = rowY.get(t.from), yb = rowY.get(t.to);
        if (yb == null) continue;
        const kind = thrKind(t), t0 = t.deliveredAt || t.createdAt, ans = t.answeredAt || t.relayedAt || null;
        const t1 = ans || (OPEN.has(t.status) ? now : null);
        const data = `data-a="${esc(t.from || "")}" data-b="${esc(t.to)}" data-thr="${esc(t.id)}"`;
        const tip = `${t.from ? raw(t.from) : "사람"} → ${raw(t.to)} · ${kind === "propose" ? "제안" : "검토"}${t.mode && t.mode !== "propose" ? ` · ${t.mode}` : ""} · ${STATUS_KO[t.status] || t.status}${t.inferred ? "\n(자동 감지된 교환)" : ""}${t0 ? `\n요청 ${hhmm(t0)}` : ""}${ans ? ` · 답 ${hhmm(ans)}` : ""}${t.magiRun ? `\nmagi ${t.magiRun}` : ""}`;
        if (t0 && t1 && t1 > from) {
          const x0 = X(Math.max(t0, from)), x1 = X(Math.min(t1, now)), top = rowTop.get(t.to);
          svg.push(`<rect class="span k-${kind}${t.inferred ? " inferred" : ""}${ans ? "" : " open"}" x="${x0.toFixed(1)}" y="${top + 20}" width="${Math.max(2, x1 - x0).toFixed(1)}" height="3" rx="1.5" ${data} data-tip="${esc(tip)}"></rect>`);
          if (t0 >= from && t.from) svg.push(`<text class="span-i k-${kind}" x="${(x0 - 1.5).toFixed(1)}" y="${top + 24}" text-anchor="end" ${data}>${esc([...t.from][0].toUpperCase())}</text>`);
        }
        if (ya == null || ya === yb) continue;
        if (t.relayedAt && t.relayedAt >= from) { const x = X(t.relayedAt), top = rowTop.get(t.from); svg.push(`<line class="rtick k-${kind}" x1="${x.toFixed(1)}" x2="${x.toFixed(1)}" y1="${top + 19}" y2="${top + 25}" ${data} data-tip="${esc(`${raw(t.to)}의 답이 ${raw(t.from)}에게 전달됨 · ${hhmm(t.relayedAt)}`)}"></line>`); }
        if (t.magiRun) { const g = runs.get(t.magiRun) || { src: t.from, at: t0, ts: [], kind, mode: t.mode }; g.ts.push(t); g.at = Math.min(g.at || t0, t0 || g.at); runs.set(t.magiRun, g); }
        else for (const tm of [t0, ans]) {
          if (!tm || tm < from) continue;
          const x = X(tm).toFixed(1);
          svg.push(`<line class="conn" x1="${x}" x2="${x}" y1="${ya}" y2="${yb}" ${data}></line>`);
        }
        const arc = (tm, y1, y2, back) => {
          if (!tm || tm < from) return;
          const x = X(tm), bend = Math.min(60, 14 + Math.abs(y2 - y1) / 4);
          curves.push(`<path class="thr-full${back ? " back" : ""}${t.inferred ? " inferred" : ""} k-${kind}" d="M${x.toFixed(1)},${y1} C${(x - bend).toFixed(1)},${y1} ${(x - bend).toFixed(1)},${y2} ${x.toFixed(1)},${y2}" marker-end="url(#arr)" ${data} data-tip="${esc(tip)}"></path>`);
        };
        arc(t0, ya, yb, false);
        arc(ans, yb, ya, true);
      }
      // magi fan-out: one bracket at the run's delivery time; zoomed out (> 6 h) it collapses into a chip.
      const collapsed = hours > 6;
      for (const [id, g] of runs) {
        const ys = g.src && rowY.has(g.src) ? g.ts.map((t) => rowY.get(t.to)).filter((y) => y != null) : [];
        if (!ys.length || !g.at || g.at < from) continue;
        const x = X(g.at), ysrc = rowY.get(g.src), lo = Math.min(ysrc, ...ys), hi = Math.max(ysrc, ...ys);
        const label = `magi ×${g.ts.length}`;
        const tip = `magi ${id} · ${g.kind === "propose" ? "제안" : "검토"}${g.mode && g.mode !== "propose" ? ` · ${g.mode}` : ""}\n${g.src} → ${g.ts.map((t) => `${t.to} (${STATUS_KO[t.status] || t.status})`).join(", ")}`;
        const fan = `<g class="fan"><line class="mg-stem" x1="${x.toFixed(1)}" x2="${x.toFixed(1)}" y1="${lo}" y2="${hi}"></line>${ys.map((y) => `<line class="mg-arm" x1="${x.toFixed(1)}" x2="${(x + 5).toFixed(1)}" y1="${y}" y2="${y}"></line>`).join("")}<circle class="mg-src" cx="${x.toFixed(1)}" cy="${ysrc}" r="2"></circle></g>`;
        const chip = collapsed ? `<g class="mg-chip"><rect x="${(x - 1).toFixed(1)}" y="${ysrc - 15}" width="${label.length * 5 + 6}" height="11" rx="5.5"></rect><text x="${(x + 2).toFixed(1)}" y="${ysrc - 6.5}">${label}</text></g>` : "";
        svg.push(`<g class="mgrp${collapsed ? " collapsed" : ""}" data-a="${esc(g.src)}" data-tip="${esc(tip)}">${chip}${fan}</g>`);
      }
    }
    // Interaction dots, drawn last so they stay the most prominent mark: my messages = filled accent dots
    // (merged when closer than 6 px; count labels only where they do not collide); agent/relay/system
    // messages = small hollow gray dots.
    for (const r of rows) {
      if (r.type !== "a") continue;
      const cy = r.y + 14, human = [], other = [];
      for (const i of r.a.inter) {
        if (i.ts < from || i.ts > now) continue;
        const firstTxt = String(i.text || "").split("\n").find((l) => l.trim()) || "";
        const who = i.kind === "human" ? "나" : i.kind === "relay" ? "릴레이" : i.kind === "agent" ? raw(i.from || "에이전트") : i.kind === "system" ? "시스템" : raw(i.from || "adash");
        (i.kind === "human" ? human : other).push({ x: X(i.ts), tip: `${hhmm(i.ts)} · ${who} → ${raw(r.a.name)}${DEMO.mask ? "" : `\n${raw(firstTxt.slice(0, 140))}`}` });
      }
      for (const o of other) svg.push(`<circle class="mk-r" cx="${o.x.toFixed(1)}" cy="${cy}" r="2.5" data-tip="${esc(o.tip)}"></circle>`);
      const clusters = [];
      for (const h of human) {
        const c = clusters[clusters.length - 1];
        if (c && h.x - c.last < 6) { c.items.push(h); c.last = h.x; } else clusters.push({ items: [h], last: h.x });
      }
      let labelRight = -Infinity;
      for (const c of clusters) {
        const x = (c.items[0].x + c.last) / 2, n = c.items.length;
        const tip = n > 1 ? `${n}개\n${c.items.map((h) => h.tip.replace("\n", "  ")).join("\n")}` : c.items[0].tip;
        svg.push(`<circle class="mk-h" cx="${x.toFixed(1)}" cy="${cy}" r="${n > 1 ? 4 : 3.5}" data-tip="${esc(tip)}"></circle>`);
        if (n > 1) {
          const w = String(n).length * 5.5;
          if (x - w / 2 > labelRight + 3) { svg.push(`<text class="mk-n" x="${x.toFixed(1)}" y="${cy - 6}" text-anchor="middle">${n}</text>`); labelRight = x + w / 2; }
        }
      }
      // Provider/API error that stopped the agent: a red diamond at its time.
      const le = r.a.s?.lastError;
      if (r.a.kind === "error" && le?.at && le.at >= from && le.at <= now) { const x = X(le.at); svg.push(`<path class="mk-err" d="M${x.toFixed(1)},${cy - 5} l5,5 l-5,5 l-5,-5 z" data-tip="${esc(errTip(le))}"></path>`); }
    }
    svg.push(`<g class="curves">${curves.join("")}</g>`);
    svg.push(`<line class="nowline" x1="${W - 0.75}" x2="${W - 0.75}" y1="0" y2="${model.height}"></line>`);
    const tl = $("#timeline");
    tl.setAttribute("height", model.height); tl.setAttribute("viewBox", `0 0 ${W} ${model.height}`);
    H("#timeline").innerHTML = svg.join("");
    if (linkHover.name || linkHover.thr) litLinks(linkHover.name, linkHover.thr, true); else if (selName) litLinks(selName, null, true);

    const f = state.priorityFormula || "(impact + 2*leverage) / cost";
    const L = [], R = [];
    for (const r of rows) {
      if (r.type === "h") {
        const open = expandedOffline.has(r.g.name);
        L.push(`<div class="hrow" data-toggle-offline="${esc(r.g.name)}" title="종료된 에이전트 (같은 이름의 이전 세션 포함) · 펼치면 표시">${open ? "오프라인 접기" : `오프라인 ${r.g.hidden.length}개 숨김`}</div>`);
        R.push(`<div class="hrow"></div>`);
        continue;
      }
      if (r.type === "g") {
        const g = r.g, sp = g.sp;
        const tip = sp.impact != null ? `${f} = (${sp.impact} + 2×${sp.leverage}) / ${sp.cost} = ${(+sp.score).toFixed(2)}` : f;
        const cls = "ghead";
        const star = model.scored ? `<span class="gm gstar" title="${esc(tip)}">★${(+(g.p.score ?? sp.score ?? 0)).toFixed(1)}</span>` : "";
        L.push(`<div class="${cls}" data-g="${esc(g.name)}"><span class="gname" title="${esc(g.name)}">${esc(g.name)}</span>${star}${sp.bottleneck ? `<span class="gm bneck"><span class="alert-dot"></span>병목</span>` : ""}<span class="spacer"></span><button class="g-more" data-proj-menu="${esc(g.name)}" title="${esc(g.name)} 숨기기·종료·보관" aria-label="프로젝트 메뉴">⋯</button><button class="g-add" data-launch-proj="${esc(g.name)}" title="${esc(g.name)}에 새 세션">+</button></div>`);
        const gtip = [`대기 ${g.waitingNow}`, g.lastHumanAt > 0 ? `내 마지막 상호작용 ${ago(g.lastHumanAt)}` : "", sp.deadline ? `마감 ${sp.deadline}` : "", sp.goal ? `목표: ${raw(sp.goal)}` : ""].filter(Boolean).join("\n");
        R.push(`<div class="${cls}" data-g="${esc(g.name)}" data-tip="${esc(gtip)}"><span class="gm">${[g.asking ? `질문 <b class="ask-n">${g.asking}</b>` : "", g.unread ? `미확인 <b class="unread-n">${g.unread}</b>` : ""].filter(Boolean).join(" · ")}</span></div>`);
        continue;
      }
      const a = r.a, s = a.s;
      const turn = a.unread && model.hasInteractions && a.kind !== "asking" && a.kind !== "error";
      const cls = `lrow${a.name === selName ? " sel" : ""}${a.live ? "" : " dimrow"}${a.unread && model.hasInteractions ? " unread" : ""}${a.kind === "asking" ? " asking" : ""}${a.kind === "error" ? " erred" : ""}${a.kind === "working" ? " work" : ""}`;
      // Left micro urgency tag (direction 1): what the row needs from me, readable without the legend.
      const utag = a.kind === "asking" ? `<span class="utag ask">ASK</span>` : a.kind === "error" && s?.state === "auth" ? `<span class="utag err">LOGIN</span>` : a.kind === "error" ? `<span class="utag err">ERR${s?.lastError?.status ? ` ${esc(s.lastError.status)}` : ""}</span>` : isMyTurn(a) ? TK(a) : "";
      L.push(`<div class="${cls}${bulk.has(a.name) ? " picked" : ""}" data-agent="${esc(a.name)}"><input type="checkbox" class="pick" tabindex="-1" ${bulk.has(a.name) ? "checked" : ""} title="여러 개 선택 (Ctrl/Shift+클릭)">${harnessBadge(s, a.live)}${a.live ? netIcon(s) : ""}
        <span class="nmwrap"><span class="nm${s?.ambiguous ? " amb" : ""}">${esc(a.name)}</span>${a.kind === "working" ? WKa(a.name) : ""}</span>${a.kind === "working" ? "" : utag}${a.live ? "" : `<button class="reopen-btn" data-reopen="${esc(a.name)}" title="저장된 세션을 이어서 새 터미널로 엶">다시 열기</button>`}</div>`);
      // Status column (v15.1): one line = [unread dot][icon chips][time][countdown][progress]. Secondary marks are
      // small same-color icon chips; if the line still overflows, fitInfoRows() folds the lowest-priority items
      // (질문 > 오류 > 결정 > 보류 > 초안 > magi > 시간) into a "+k" chip whose tooltip lists everything.
      const chip = (p, cls, icon, n, tip) => `<span class="ic ${cls}" data-p="${p}" data-tip="${esc(tip)}">${ICONS[icon]}${n != null && n !== "" ? `<b>${esc(n)}</b>` : ""}</span>`;
      let st;
      // Core state (.st-c) is always kept; auxiliary text (.st-a: tool, elapsed time) shrinks with "…" when the
      // column is short after the optional marks have folded.
      if (a.kind === "asking") st = `<span class="st-txt ask" data-p="0" data-tip="${esc(`질문 대기 ${fmtMin(a.waitedMin) || "0분"}`)}"><span class="st-c">? 질문</span><span class="st-a"> · ${fmtMin(a.waitedMin) || "0분"}</span></span>`;
      else if (a.kind === "error" && s?.state === "auth") st = `<span class="st-txt" data-p="0" data-tip="${esc(s.auth?.reason || "")}"><span class="st-c err-n">${t_("로그인 필요")}</span></span>`;
      else if (a.kind === "error") { const e = s?.lastError; st = `<span class="st-txt" data-p="0" data-tip="${esc(errTip(e))}"><span class="st-a">${e?.at ? ago(e.at) : "멈춤"}</span></span>`; }
      else if (a.kind === "waiting") st = `<span class="st-txt${turn ? " turn" : ""}" data-p="0" data-tip="${esc(`${fmtMin(a.waitedMin)} 전에 답함`)}"><span class="st-c">${fmtMin(a.waitedMin)} 전 답</span></span>`;
      else if (a.kind === "snoozed") {
        // Gray: what it would be (질문 / 오류 / 미확인) + until when, and an unsnooze button. No colour, no weight.
        const until = fmtUntil(a.snoozedUntil);
        const what = { asking: L2("질문", "Question"), error: s?.lastError ? errLabel(s.lastError) : L2("오류", "Error"), unread: L2("미확인", "Unread") }[a.held] || "";
        st = `<span class="st-txt snz" data-p="0" data-tip="${esc(L2(`${until}까지 미룸 · 강조 끔`, `Snoozed until ${until} · highlights off`))}"><span class="st-c">${what ? `${esc(what)} · ` : ""}${esc(L2(`미룸 ~${until}`, `Snoozed ~${until}`))}</span></span><button type="button" class="unsnz-btn" data-p="0" data-unsnooze="${esc(a.name)}">${esc(L2("해제", "Unsnooze"))}</button>`;
      }
      else if (a.kind === "working") st = `<span class="st-txt work" data-p="0" data-tip="${esc(`작업 중${s?.tool ? ` · ${s.tool}` : ""}`)}"><span class="st-c">작업 중</span>${s?.tool ? `<span class="st-a"> · <code>${esc(s.tool)}</code></span>` : ""}</span>`;
      else { const t = a.lastHumanAt ? ago(a.lastHumanAt) : SEG_KO[a.kind] || a.kind; st = `<span class="st-txt idle" data-p="0" data-tip="${esc(a.lastHumanAt ? `내 마지막 메시지 ${t}` : t)}"><span class="st-a">${esc(t)}</span></span>`; }
      const chips = [];
      const lastErr = a.kind === "error" ? s?.lastError || {} : null;
      // Status column keeps the retry button icon-only (↻) to fit next to "오류 403"; its tooltip names the harness method.
      if (lastErr && s?.state === "auth") chips.push(`<button class="retry-btn" data-p="0" data-act="restart-agent" data-name="${esc(a.name)}" data-tip="${esc(t_("다시 시작 (다른 창에서 로그인했다면)"))}" aria-label="${esc(t_("다시 시작"))}">${ICONS.retry}</button>`);
      else if (lastErr) chips.push(`<span class="ic c-err c-err-main" data-p="0" data-tip="${esc(errTip(s?.lastError))}">${ICONS.err}<b>${esc(errLabel(s?.lastError))}</b></span><button class="retry-btn" data-p="0" data-retry="${esc(a.name)}" data-tip="${esc(retryTip(s))}" aria-label="${esc(retryLabel(s))}">${ICONS.retry}</button>`);
      const bad = !lastErr && a.kind !== "snoozed" && ((state.threads || []).some((t) => (t.from === a.name || t.to === a.name) && ["escalated", "failed"].includes(t.status) && now - (t.createdAt || 0) < 86400000) || (typeof s?.error === "string" && s.error));
      if (bad) chips.push(chip(2, "c-err", "err", "", `오류 · ${typeof s?.error === "string" ? s.error : "실패하거나 에스컬레이션된 검토 요청이 있음"}`));
      const decs = decisionsOf(s);
      if (decs.length) chips.push(chip(4, "c-dec", "dec", decs.length, `정하실 것 ${decs.length}\n${decs.map((d, i) => `${i + 1}. ${raw(d.text)}`).join("\n")}`));
      const hr = heldRelays(a.name);
      if (hr.length) chips.push(chip(5, "c-held", "held", hr.length > 1 ? hr.length : "", `답변 보류 ${hr.length}\n${hr.map((t) => `${t.to}의 검토 답변 · ${HELD_KO[t.heldReason] || t.heldReason || ""}`).join("\n")}`));
      if (s?.draft?.text) chips.push(chip(6, "c-draft", "draft", "", `터미널 입력창 초안\n${raw(s.draft.text.split("\n")[0].slice(0, 120))}`));
      const mg = magiRunsOf(a.name);
      const mgDone = (r) => (r.targets || []).filter((t) => ["answered", "relay_held", "relayed"].includes(t.status)).length;
      if (mg.length) chips.push(chip(9, `c-magi ${magiPhaseCls(mg[0].phase)}`, "magi", mg[0].targets?.length ? `${mgDone(mg[0])}/${mg[0].targets.length}` : "", mg.map((r) => `magi ${MAGI_KIND_KO[r.kind] || r.kind} · ${MAGI_PHASE_KO[r.phase] || r.phase}${r.heldReason && r.status === "held" ? ` (${MAGI_HELD_KO[r.heldReason] || r.heldReason})` : ""}\n${(r.targets || []).map((t) => `  ${t.name} · ${MAGI_TSTATUS_KO[t.status] || t.status}`).join("\n")}`).join("\n")));
      const pr = s?.progress;
      const prog = `<span class="prog" data-p="8"${pr && pr.total ? ` data-tip="${esc(`진행 ${pr.done}/${pr.total}${pr.current ? ` · ${raw(pr.current)}` : ""}`)}"` : ""}>${pr && pr.total ? `${pr.done}/${pr.total}` : ""}</span>`;
      // Cache countdown while an answer waits on me (A4); ticked every second by tickCountdowns().
      const exp = (a.unread || a.kind === "asking" || a.kind === "waiting") && cacheState(s) !== "cold" ? cacheExp(s) : null;
      // Cache countdown is informational (gray, clock glyph), labelled in its tooltip.
      const cd = exp ? `<span class="cd" data-p="7" data-exp="${exp}" data-basis="${esc(cacheBasis(s))}" data-label="캐시 만료 예상"></span>` : "";
      R.push(`<div class="${cls}" data-agent="${esc(a.name)}">${a.kind === "error" ? `${chips.join("")}${st}` : `${st}${chips.join("")}`}<span class="spacer"></span><span class="ic c-more" hidden></span>${cd}${prog}</div>`);
    }
    H("#lane-labels").innerHTML = L.join("");
    if (!document.documentElement.style.getPropertyValue("--tk-tw") || measureTkSlot.lang !== window.ADASH_LANG) measureTkSlot();
    placeMarkFx($("#lane-labels"));
    renderBoardEmpty();
    H("#lane-info").innerHTML = R.join("");
    renderHiddenProj();
    fitInfoRows(); refit();
  }

  // Empty / degraded board states (v40): first run (no agents at all) and a filter with nothing to show.
  function renderBoardEmpty() {
    const el = $("#board-empty");
    if (!el) return;
    const shown = model.rows.some((r) => r.type === "a");
    let html = "";
    const tmuxDown = state?.backend?.available === false;
    if (!state) html = connDownSince != null ? `<div class="empty-card"><b>아직 상태를 받지 못했습니다</b><span>adash 서버 연결을 확인하세요.</span><button data-act="recheck">다시 연결</button></div>` : "";
    // Could not look: never read an empty snapshot as "no agents" / "all caught up".
    else if (!shown && tmuxDown) html = `<div class="empty-card"><b>tmux 서버가 응답하지 않아 에이전트 목록을 확인할 수 없습니다</b><span>연결이 복구되면 다시 조회합니다.</span><button data-act="recheck">지금 다시 확인</button></div>`;
    else if (!shown && rawAgentCount === 0 && !model.agents.length)
      html = `<div class="empty-card"><b>adash 전용 tmux 서버에서 실행 중인 에이전트가 없습니다</b><span>새 세션을 시작하거나 지난 세션을 이어서 여세요. 둘러보기만 하려면 <code>adash demo</code>.</span><code>adash term new --cwd ~/work/app --command "claude"</code><div class="row"><button class="primary" data-act="m-new">새 세션 시작 (n)</button><button data-act="m-sessions">지난 세션</button></div></div>`;
    else if (!shown && !model.agents.some((a) => a.live) && hiddenProjects.length)
      html = `<div class="empty-card"><b>표시 중인 프로젝트가 없습니다</b><span>숨긴 프로젝트의 에이전트는 계속 실행될 수 있습니다. 아래 '숨긴 프로젝트'에서 다시 보이게 할 수 있습니다.</span></div>`;
    else if (!shown) {
      // "All caught up" only when the top bar's own My-turn count is 0 (same computation, not a second rule).
      const needs = model.counts?.mine || 0;
      const work = model.agents.filter((a) => a.live && a.kind === "working").length, idle = model.agents.filter((a) => a.live && a.kind === "idle").length;
      html = needs === 0
        ? `<div class="empty-card"><b>모두 확인했습니다</b><span>지금 나를 기다리는 에이전트가 없습니다 (작업 중 ${work}개 · 유휴 ${idle}개)</span><button data-act="filter-all">전체 에이전트 보기</button></div>`
        : `<div class="empty-card"><b>현재 필터에 맞는 에이전트가 없습니다</b><span>${L2(`질문·오류를 포함한 전체 상태는 '전체 에이전트 보기'에서 확인하세요. 지금 나를 기다리는 에이전트 ${needs}개.`, `See every state, including questions and errors, with 'Show all agents'. ${needs} agent${needs === 1 ? " is" : "s are"} waiting on you.`)}</span><button data-act="filter-all">전체 에이전트 보기</button></div>`;
    }
    el.hidden = !html;
    if (html) H("#board-empty").innerHTML = html;
  }

  // Agent row under a pointer event on the timeline SVG (y in SVG user units = CSS px inside the zoomed lanes).
  function rowAt(e) {
    const svg = $("#timeline"), box = svg.getBoundingClientRect();
    const y = ((e.clientY - box.top) / box.height) * model.height;
    return model.rows.find((r) => r.type === "a" && y >= r.y && y < r.y + r.h)?.a.name || null;
  }
  // Row tooltip: the detail removed from the right column (last message, my last message, progress).
  function rowTip(name) {
    const a = model.agents.find((x) => x.name === name);
    if (!a) return "";
    const s = a.s, out = [];
    const head = `${raw(a.name)}${a.role ? ` · ${raw(a.role)}` : ""} · ${SEG_KO[a.kind] || a.kind}${a.kind === "waiting" && cacheState(s) ? ` · 캐시 ${cacheState(s)}` : ""}${a.unread ? " · 미확인 답변" : ""}`;
    out.push(head);
    // Masked (recording) mode: name, state and counts only, no message or question text.
    if (DEMO.mask) { const pr = s?.progress, dn = decisionsOf(s).length; if (dn) out.push(`정하실 것 ${dn}`); if (pr && pr.total) out.push(`진행 ${pr.done}/${pr.total}`); return out.join("\n"); }
    if (s?.ask) {
      const qs = askQuestions(s.ask);
      qs.forEach((x, qi) => {
        out.push("", `질문${qs.length > 1 ? ` ${qi + 1}/${qs.length}` : ""}${x.multi ? " (여러 개 선택)" : ""}`, ...String(x.question || "").split("\n").filter((l) => l.trim()).slice(0, 3).map(raw));
        x.options.forEach((o, i) => out.push(`  ${i + 1}. ${raw(`${o.label}${o.description ? ` — ${o.description}` : ""}`)}`));
      });
    }
    const lines = String(s?.lastMessage || "").split("\n").map((l) => l.trim()).filter(Boolean).slice(0, 4).map((l) => (l.length > 110 ? `${l.slice(0, 109)}…` : l));
    if (lines.length) out.push("", "마지막 메시지", ...lines.map(raw));
    if (a.lastHuman) out.push("", `내가 보낸 말 (${ago(a.lastHuman.ts)})`, ...String(a.lastHuman.text || "").split("\n").filter((l) => l.trim()).slice(0, 2).map((l) => raw(l.length > 110 ? `${l.slice(0, 109)}…` : l)));
    const decs = decisionsOf(s);
    if (decs.length) out.push("", `정하실 것 ${decs.length}`, ...decs.map((d, i) => `  ${i + 1}. ${raw(`${d.text}${d.options?.length ? ` (${d.options.join(" / ")})` : ""}`)}`));
    if (s?.draft?.text) out.push("", "터미널 입력창 초안", `  ${raw(s.draft.text.split("\n")[0].slice(0, 100))}`);
    const pr = s?.progress;
    if (pr && pr.total) out.push("", `진행 ${pr.done}/${pr.total}${pr.current ? ` · ${raw(pr.current)}` : ""}`);
    return out.join("\n");
  }
  {
    const tip = $("#tip");
    const show = (text, e) => {
      if (!text) { tip.hidden = true; return; }
      if (tip.textContent !== text) tip.textContent = text;
      tip.hidden = false;
      const x = Math.min(e.clientX + 14, window.innerWidth - tip.offsetWidth - 8);
      const y = e.clientY + 16 + tip.offsetHeight > window.innerHeight ? e.clientY - tip.offsetHeight - 10 : e.clientY + 16;
      tip.style.left = `${x}px`; tip.style.top = `${y}px`;
    };
    // Hovering a row in any column highlights the same agent in the other columns.
    // Connections of the hovered row (or hovered span) light up at full strength with both rows marked;
    // otherwise the selected row keeps its connections lit.
    const hover = (name, thr) => {
      $$(".lrow").forEach((el) => el.classList.toggle("hover", !!name && el.dataset.agent === name));
      if (linkHover.name === name && linkHover.thr === thr) return;
      linkHover = { name, thr };
      if (thr || name) litLinks(name, thr); else litLinks(selName, null);
    };
    $("#lanes").addEventListener("mousemove", (e) => {
      const r = e.target.closest(".lrow[data-agent]");
      const thr = e.target.closest("#timeline [data-thr]")?.dataset.thr || null;
      const name = r ? r.dataset.agent : e.target.closest("#timeline") ? rowAt(e) : null;
      hover(name, thr);
      const t = e.target.closest("[data-tip]");
      show(t ? t.dataset.tip : name ? rowTip(name) : "", e);
    });
    $("#lanes").addEventListener("mouseleave", () => { hover(null); tip.hidden = true; });
  }

  function jumpToProject(name) {
    const g = model.groups.find((x) => x.name === name);
    if (!g) return;
    const el = $(`.ghead[data-g="${CSS.escape(name)}"]`);
    if (el) window.scrollTo({ top: Math.max(0, window.scrollY + el.getBoundingClientRect().top - 110), behavior: "smooth" });
    $$(`.ghead[data-g="${CSS.escape(name)}"]`).forEach((el) => { el.classList.add("flash"); setTimeout(() => el.classList.remove("flash"), 900); });
  }

  // ---------- AI capacity strip (GET /api/capacity, from `omp usage`) ----------
  let capacity = null, capErr = null, capLoading = false;
  const VENDOR_KO = { claude: "Claude", openai: "OpenAI", google: "Google", other: "기타" };
  const pct = (f) => `${Math.round((f || 0) * 100)}%`;
  const levelOf = (f) => (f > 0.9 ? "crit" : f > 0.7 ? "warn" : "");
  function resetTxt(t) {
    if (!t) return "";
    const m = (t - now()) / 60000;
    if (m <= 0) return "초기화됨";
    const d = Math.floor(m / 1440), h = Math.floor((m % 1440) / 60), mm = Math.round(m % 60);
    return `초기화 ${d ? `${d}일 ` : ""}${h ? `${h}시간 ` : ""}${d ? "" : `${mm}분 `}후`;
  }
  async function refreshCapacity(force) {
    if (capLoading) return;
    capLoading = true; if (force) renderCapacity();
    try { capacity = await api("GET", `/api/capacity${force ? "?refresh=1" : ""}`, undefined, { quiet: true }); capErr = null; }
    catch (e) { capErr = e.message; }
    capLoading = false;
    renderCapacity();
  }
  // Compact capacity (item 4): one cluster at the right end of the bar, worst window per vendor
  // ("Claude 62% · OpenAI 47% · Google 20%"); every window and account is in the hover text.
  function capVendors() {
    const ORDER = { claude: 0, openai: 1, google: 2 };
    const vs = [...(capacity?.vendors || [])].sort((a, b) => (ORDER[a.vendor] ?? 9) - (ORDER[b.vendor] ?? 9));
    return vs.map((v) => {
      const lims = (v.accounts || []).flatMap((a) => (a.limits || []).map((l) => ({ ...l, account: a.account })));
      const worst = lims.reduce((m, l) => (!m || (l.usedFraction || 0) > (m.usedFraction || 0) ? l : m), null);
      const tip = [`${VENDOR_KO[v.vendor] || v.vendor} (${v.provider})`, ...(v.accounts || []).flatMap((a) => [`${a.account}${a.plan ? ` · ${a.plan}` : ""}`, ...(a.limits || []).map((l) => `  ${l.label}${l.windowLabel && l.windowLabel !== l.label ? ` (${l.windowLabel})` : ""} ${pct(l.usedFraction)}${l.status && l.status !== "ok" ? ` · ${l.status}` : ""} · ${resetTxt(l.resetsAt)}`)])].join("\n");
      return { name: VENDOR_KO[v.vendor] || v.provider || v.vendor, main: v.vendor in ORDER, worst, tip };
    }).filter((v) => v.worst);
  }
  function renderCapacity() {
    const el = $("#capacity");
    if (!capacity) { H("#capacity").innerHTML = capErr ? `<span class="muted" title="${esc(capErr)}">용량 ?</span>` : capLoading ? `<span class="muted">용량…</span>` : ""; return; }
    const vs = capVendors(), main = vs.filter((v) => v.main), extra = vs.filter((v) => !v.main);
    const age = capacity.fetchedAt ? fmtMin(Math.max(0, (now() - capacity.fetchedAt) / 60000)) || "0분" : "";
    const foot = `\n\n${age ? `${age} 전 기준` : ""}${capacity.stale || capErr ? " · 오래됨" : ""} · 누르면 새로고침`;
    el.classList.toggle("stale", !!capacity.stale || !!capErr);
    H("#capacity").innerHTML = main.map((v) => `<span class="cap-v ${levelOf(v.worst.usedFraction)}" data-tip="${esc(v.tip + foot)}">${esc(v.name)} <b>${pct(v.worst.usedFraction)}</b></span>`).join(`<span class="cap-sep">·</span>`) +
      (extra.length ? `<span class="cap-more" data-tip="${esc(extra.map((v) => v.tip).join("\n\n") + foot)}">+${extra.length}</span>` : "") + (capLoading ? `<span class="muted"> …</span>` : "");
  }
  // Phone 더보기: the same numbers, one line per vendor.
  const capListHtml = () => (capacity ? `<div class="m-cap"><b>AI 용량</b>${capVendors().map((v) => `<div class="m-cap-r ${levelOf(v.worst.usedFraction)}" title="${esc(v.tip)}"><span>${esc(v.name)}</span><span class="muted">${esc(v.worst.label || "")}</span><b>${pct(v.worst.usedFraction)}</b></div>`).join("")}</div>` : "");
  {
    const tip = $("#tip"), el = $("#capacity");
    el.addEventListener("mousemove", (e) => {
      const t = e.target.closest("[data-tip]");
      if (!t) { tip.hidden = true; return; }
      if (tip.textContent !== t.dataset.tip) tip.textContent = t.dataset.tip;
      tip.hidden = false;
      tip.style.left = `${Math.min(e.clientX + 14, window.innerWidth - tip.offsetWidth - 8)}px`; tip.style.top = `${e.clientY + 16}px`;
    });
    el.addEventListener("mouseleave", () => { tip.hidden = true; });
    el.addEventListener("click", () => refreshCapacity(true));
  }

  // ---------- new session / worktree / project (opens an adash tmux terminal) ----------
  const ld = { mode: "existing", ws: null, cat: null, target: null, pending: null };
  const ldMode = (m) => {
    ld.mode = m;
    $$("[data-ld-mode]").forEach((b) => b.classList.toggle("on", b.dataset.ldMode === m));
    $$("[data-ld-pane]").forEach((s) => (s.hidden = s.dataset.ldPane !== m));
    // A session name/role is only part of the session contract.
    $$("#ld-name, #ld-role").forEach((i) => (i.closest(".ld-row").hidden = m !== "existing"));
  };
  // A project is a git repo when one of its worktrees has a branch; worktree basename names linked worktrees.
  const ldIsGit = (p) => p.worktrees.some((w) => w.branch);
  const ldBase = (p) => String(p || "").split("/").filter(Boolean).pop() || p;
  function renderLdList() {
    const q = $("#ld-search").value.trim().toLowerCase();
    const projects = ld.ws?.projects || [];
    H("#ld-list").innerHTML = projects.map((r) => {
      const wts = r.worktrees.filter((w) => !q || `${r.name} ${r.parent || ""} ${ldBase(w.path)} ${w.branch || ""} ${w.path}`.toLowerCase().includes(q));
      if (!wts.length) return "";
      // a board project in a subfolder of a repository: "paper-draft › figures"
      const title = r.parent ? `<span class="muted">${esc(r.parent)} ›</span> ${esc(r.name)}` : esc(r.name);
      return `<div class="ld-repo"><div class="ld-rname">${title} <span class="muted">${r.parent ? esc(r.subPath || "") : ldIsGit(r) ? "git" : "폴더"}</span></div>${wts.map((w) => `<button type="button" class="ld-wt${ld.target === w.path ? " on" : ""}" data-ld-wt="${esc(w.path)}" title="${esc(w.path)}"><span>${esc(w.isMain ? r.name : ldBase(w.path))}</span><span class="muted">${w.branch ? esc(w.branch) : ""}${w.liveAgents ? ` · 에이전트 ${w.liveAgents}` : ""}</span></button>`).join("")}</div>`;
    }).join("") || `<p class="muted">일치하는 프로젝트 없음</p>`;
  }
  // Model field: a searchable list of every model the harness offers (a <datalist> would hide all but the entries
  // matching the current value). The full list opens on focus; typing filters it; groups by provider.
  const ldAgent = () => ld.cat?.agents.find((x) => x.id === $("#ld-agent").value);
  function ldEfforts() {
    const a = ldAgent(); if (!a) return;
    const m = $("#ld-model").value.trim().split(":")[0];
    const list = (a.effortsByModel && a.effortsByModel[m]) || (m ? [] : a.efforts) || [];
    const keep = $("#ld-effort").value;
    $("#ld-effort-row").hidden = a.effortMode === "none" || !list.length;
    $("#ld-effort").innerHTML = `<option value="">${L2("(기본)", "(default)")}</option>` + list.map((e) => `<option${e === keep ? " selected" : ""}>${esc(e)}</option>`).join("");
  }
  function ldModelList(filter) {
    const a = ldAgent(), box = $("#ld-model-list"); if (!a) return;
    const q = (filter || "").trim().toLowerCase(), cur = $("#ld-model").value.trim();
    const items = (a.models || []).filter((m) => !q || `${m} ${a.labels?.[m] || ""}`.toLowerCase().includes(q));
    const groups = new Map();
    for (const m of items) { const g = m.includes("/") ? m.split("/")[0] : a.label || a.id; if (!groups.has(g)) groups.set(g, []); groups.get(g).push(m); }
    box.innerHTML = items.length ? [...groups].map(([g, ms]) => `<div class="ld-combo-g">${esc(g)}</div>` + ms.map((m) => {
      const label = a.labels?.[m] && a.labels[m] !== m ? `<span class="muted">${esc(a.labels[m])}</span>` : "";
      return `<div class="ld-combo-o${m === cur ? " on" : ""}" role="option" data-ld-model="${esc(m)}"><span>${esc(m.includes("/") ? m.split("/").slice(1).join("/") : m)}</span>${label}</div>`;
    }).join("")).join("") : `<div class="ld-combo-empty muted">${L2("일치하는 모델 없음 · 입력한 값 그대로 사용", "no match · the typed value is used as is")}</div>`;
    box.hidden = false; $("#ld-model").setAttribute("aria-expanded", "true");
    // inside the dialog: open toward the side with room (the dialog clips what overflows it)
    const inp = $("#ld-model").getBoundingClientRect(), dlg = $("#launch-dlg").getBoundingClientRect();
    const below = dlg.bottom - inp.bottom - 10, above = inp.top - dlg.top - 10, up = below < 180 && above > below;
    box.classList.toggle("up", up);
    box.style.maxHeight = `${Math.max(120, Math.min(320, up ? above : below))}px`;
    box.querySelector(".on")?.scrollIntoView({ block: "nearest" });
  }
  const ldModelClose = () => { $("#ld-model-list").hidden = true; $("#ld-model").setAttribute("aria-expanded", "false"); };
  function ldAgentChanged() {
    const a = ldAgent();
    if (!a) return;
    $("#ld-model").value = a.defaultModel || "";
    ldModelClose();
    $("#ld-effort").value = "";
    ldEfforts();
  }
  async function openLaunch(presetProject, preset = {}) {
    const dlg = $("#launch-dlg");
    $("#ld-result").textContent = ""; $("#ld-submit").disabled = false;
    ldMode(presetProject === "__new_project" ? "project" : "existing");
    if (!dlg.open) dlg.showModal();
    $("#ld-search").value = "";
    try {
      [ld.ws, ld.cat] = await Promise.all([api("GET", "/api/workspaces", undefined, { quiet: true }), api("GET", "/api/launch/catalog", undefined, { quiet: true })]);
    } catch (e) { $("#ld-result").textContent = `불러오기 실패: ${e.message}`; $("#ld-submit").disabled = true; return; }
    // Default target: the selected row's worktree, else the preset/selected project's main worktree.
    const sel = selName && agentBy(selName);
    const proj = presetProject || sel?.project;
    const all = ld.ws.projects.flatMap((r) => r.worktrees.map((w) => ({ r, w })));
    const hit = (!presetProject && sel && all.find((x) => x.w.path === sel.worktreePath)) || all.find((x) => x.r.name === proj && x.w.isMain) || all.find((x) => x.r.name === proj);
    ld.target = hit?.w.path || null;
    $("#ld-repo").innerHTML = ld.ws.projects.filter(ldIsGit).map((r) => `<option value="${esc(r.name)}" ${hit?.r.name === r.name ? "selected" : ""}>${esc(r.name)}</option>`).join("");
    $("#ld-agent").innerHTML = ld.cat.agents.filter((a) => a.installed).map((a) => `<option value="${esc(a.id)}">${esc(a.label || a.id)}</option>`).join("");
    if (preset.agent && ld.cat.agents.some((x) => x.id === preset.agent && x.installed)) $("#ld-agent").value = preset.agent;
    ldAgentChanged();
    if (preset.model) { $("#ld-model").value = preset.model; ldEfforts(); }
    if (preset.name) $("#ld-name").value = preset.name;
    if (preset.role) $("#ld-role").value = preset.role;
    renderLdList();
    $(`#ld-list [data-ld-wt="${CSS.escape(ld.target || "")}"]`)?.scrollIntoView({ block: "nearest" });
    (ld.mode === "existing" ? $("#ld-search") : $("#ld-path")).focus();
  }
  async function submitLaunch() {
    const v = (id) => $(id).value.trim();
    const common = { agent: v("#ld-agent"), focus: $("#ld-focus").checked, autoTrust: $("#ld-trust").checked };
    if (v("#ld-model")) common.model = v("#ld-model");
    if (!$("#ld-effort-row").hidden && v("#ld-effort")) common.effort = v("#ld-effort");
    const prompt = $("#ld-prompt").value.replace(/\s+$/, "");
    if (prompt.trim()) common.prompt = prompt;
    let path, body;
    if (ld.mode === "existing") {
      if (!ld.target) { $("#ld-result").textContent = "대상 프로젝트를 고르세요"; return; }
      path = "/api/sessions"; body = { path: ld.target, ...common };
      if (v("#ld-name")) body.name = v("#ld-name");
      if (v("#ld-role")) body.role = v("#ld-role");
    } else if (ld.mode === "worktree") {
      if (!v("#ld-repo") || !v("#ld-wt-name")) { $("#ld-result").textContent = "저장소와 이름을 입력하세요"; return; }
      path = "/api/worktrees"; body = { project: v("#ld-repo"), name: v("#ld-wt-name"), ...common };
      if (v("#ld-wt-base")) body.baseBranch = v("#ld-wt-base");
    } else {
      if (!v("#ld-path")) { $("#ld-result").textContent = "경로를 입력하세요"; return; }
      path = "/api/projects"; body = { path: v("#ld-path"), create: $("#ld-create").checked, gitInit: $("#ld-git").checked, ...common };
    }
    $("#ld-submit").disabled = true; $("#ld-result").textContent = "여는 중…";
    try {
      const r = await api("POST", path, body);
      ld.pending = { handle: r?.handle || null, name: body.name || null, until: Date.now() + 90000 };
      $("#ld-result").textContent = `열림 ✓ ${r?.handle || ""}${r?.worktree?.branch ? ` · ${r.worktree.branch}` : ""}`;
      ["#ld-prompt", "#ld-name", "#ld-role", "#ld-wt-name", "#ld-wt-base", "#ld-path"].forEach((id) => ($(id).value = ""));
      setTimeout(() => { if ($("#launch-dlg").open) $("#launch-dlg").close(); }, 1500);
      if (!FIXTURE) refresh();
    } catch (e) { $("#ld-result").textContent = `실패: ${e.message}`; $("#ld-submit").disabled = false; }
  }
  // When the launched pane shows up in /api/state, select and highlight its row.
  function highlightLaunched() {
    const p = ld.pending;
    if (!p) return;
    if (Date.now() > p.until) { ld.pending = null; return; }
    const a = state.agents.find((x) => (p.handle && x.handle === p.handle) || (p.name && x.name === p.name));
    if (!a) return;
    ld.pending = null; selName = a.name; render();
    requestAnimationFrame(() => $$(`.lrow[data-agent="${CSS.escape(a.name)}"]`).forEach((el) => { el.scrollIntoView({ block: "center" }); el.classList.add("flash"); setTimeout(() => el.classList.remove("flash"), 1600); }));
    toast(`새 세션: ${a.name}`, 3000);
  }
  $("#new-session").addEventListener("click", () => openLaunch());
  $("#past-sessions").addEventListener("click", () => openSessions());
  $("#launch-form").addEventListener("submit", (e) => { e.preventDefault(); submitLaunch(); });
  $("#launch-dlg").addEventListener("click", (e) => {
    if (e.target.closest("[data-ld-close]")) return $("#launch-dlg").close();
    const m = e.target.closest("[data-ld-mode]"); if (m) return ldMode(m.dataset.ldMode);
    const w = e.target.closest("[data-ld-wt]"); if (w) { ld.target = w.dataset.ldWt; renderLdList(); }
  });
  $("#ld-search").addEventListener("input", renderLdList);
  $("#ld-agent").addEventListener("change", ldAgentChanged);
  $("#ld-model").addEventListener("focus", () => ldModelList(""));
  $("#ld-model").addEventListener("click", () => { if ($("#ld-model-list").hidden) ldModelList(""); });
  $("#ld-model").addEventListener("input", (e) => { ldModelList(e.target.value); ldEfforts(); });
  $("#ld-model").addEventListener("keydown", (e) => {
    const box = $("#ld-model-list"), opts = [...box.querySelectorAll(".ld-combo-o")];
    if (e.key === "Escape" && !box.hidden) { e.preventDefault(); e.stopPropagation(); ldModelClose(); return; }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Enter") return;
    if (box.hidden) { if (e.key !== "Enter") { e.preventDefault(); ldModelList(""); } return; }
    const i = opts.findIndex((o) => o.classList.contains("hl"));
    if (e.key === "Enter") { if (i >= 0) { e.preventDefault(); opts[i].click(); } else ldModelClose(); return; }
    e.preventDefault();
    const n = opts.length ? (i + (e.key === "ArrowDown" ? 1 : -1) + opts.length) % opts.length : -1;
    opts.forEach((o, k) => o.classList.toggle("hl", k === n));
    opts[n]?.scrollIntoView({ block: "nearest" });
  });
  $("#ld-model-list").addEventListener("mousedown", (e) => {
    const o = e.target.closest("[data-ld-model]"); if (!o) return;
    e.preventDefault(); $("#ld-model").value = o.dataset.ldModel; ldModelClose(); ldEfforts();
  });
  $("#ld-model").addEventListener("blur", () => setTimeout(ldModelClose, 120));
  $("#ld-prompt").addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); submitLaunch(); } });

  // ---------- new project path: directory autocomplete (v46; GET /api/fs/dirs?q= → {query, exists, items:[{name, path, exists, isGit}]}) ----------
  const ldp = { seq: 0, t: 0, items: [] };
  const FIX_DIRS = ["~/work", "~/work/paper-draft", "~/work/grant-app", "~/work/adash-dev", "~/code", "~/code/notes", "~/projects", "~/projects/climate-model"];
  async function fsDirs(q) {
    if (!FIXTURE) return api("GET", `/api/fs/dirs?q=${enc(q)}`);
    const cut = q.lastIndexOf("/"), par = q.slice(0, cut + 1), pre = q.slice(cut + 1).toLowerCase();
    const items = (q ? FIX_DIRS.filter((p) => p.startsWith(par) && !p.slice(par.length).includes("/") && p.slice(par.length).toLowerCase().startsWith(pre)) : FIX_DIRS.filter((p) => p.split("/").length === 2))
      .map((p) => ({ name: p.split("/").pop(), path: p, exists: true, isGit: /paper|grant|adash|climate/.test(p) }));
    return { query: q, exists: FIX_DIRS.includes(q.replace(/\/+$/, "")), items };
  }
  const ldPathClose = () => { $("#ld-path-list").hidden = true; $("#ld-path").setAttribute("aria-expanded", "false"); };
  function ldPathStatus(r) {
    const v = $("#ld-path").value.trim(), st = $("#ld-path-st");
    st.textContent = !v || !r ? "" : r.exists ? L2("기존 폴더 · 그 폴더에서 엽니다", "existing folder · opens there") : L2("새 폴더", "new folder");
    st.classList.toggle("exists", !!(v && r?.exists));
  }
  async function ldPathList() {
    const q = $("#ld-path").value, seq = ++ldp.seq;
    let r;
    try { r = await fsDirs(q); } catch { if (seq === ldp.seq) { ldPathClose(); ldPathStatus(null); } return; }
    if (seq !== ldp.seq || document.activeElement !== $("#ld-path")) return;
    ldPathStatus(r);
    ldp.items = r?.items || [];
    const box = $("#ld-path-list");
    // An exact existing folder with nothing under it: no list, just the status line.
    if (!ldp.items.length) { ldPathClose(); return; }
    box.innerHTML = (q ? "" : `<div class="ld-combo-g">${L2("자주 쓰는 폴더", "Common folders")}</div>`) + ldp.items.map((d, i) => `<div class="ld-combo-o" role="option" data-ld-dir="${i}"><span>${esc(d.name)}/</span>${d.isGit ? `<span class="muted">git</span>` : ""}<span class="ld-dir-p muted">${esc(d.path)}</span></div>`).join("");
    box.hidden = false; $("#ld-path").setAttribute("aria-expanded", "true");
    const inp = $("#ld-path").getBoundingClientRect(), dlg = $("#launch-dlg").getBoundingClientRect();
    const below = dlg.bottom - inp.bottom - 10, above = inp.top - dlg.top - 10, up = below < 180 && above > below;
    box.classList.toggle("up", up);
    box.style.maxHeight = `${Math.max(120, Math.min(320, up ? above : below))}px`;
  }
  const ldPathSoon = () => { clearTimeout(ldp.t); ldp.t = setTimeout(ldPathList, 120); };
  // Enter a folder: path + "/" and list its children; the last (new) name stays free text.
  function ldPathEnter(i) {
    const d = ldp.items[i]; if (!d) return;
    const inp = $("#ld-path"); inp.value = `${d.path}/`; inp.focus(); inp.setSelectionRange(inp.value.length, inp.value.length);
    ldPathList();
  }
  $("#ld-path").addEventListener("focus", ldPathSoon);
  $("#ld-path").addEventListener("input", ldPathSoon);
  $("#ld-path").addEventListener("keydown", (e) => {
    if (e.isComposing) return;
    const box = $("#ld-path-list"), opts = [...box.querySelectorAll(".ld-combo-o")];
    if (e.key === "Escape" && !box.hidden) { e.preventDefault(); e.stopPropagation(); ldPathClose(); return; }
    if (!["ArrowDown", "ArrowUp", "Enter", "Tab"].includes(e.key) || e.shiftKey) return;
    if (box.hidden) { if (e.key.startsWith("Arrow")) { e.preventDefault(); ldPathList(); } return; }
    const i = opts.findIndex((o) => o.classList.contains("hl"));
    if (e.key === "Enter" || e.key === "Tab") {
      // Tab with nothing highlighted takes the first match (shell-style); Enter only acts on a highlighted row.
      const k = i >= 0 ? i : e.key === "Tab" ? 0 : -1;
      if (k >= 0) { e.preventDefault(); ldPathEnter(+opts[k].dataset.ldDir); } else ldPathClose();
      return;
    }
    e.preventDefault();
    const n = (i + (e.key === "ArrowDown" ? 1 : -1) + opts.length) % opts.length;
    opts.forEach((o, k) => o.classList.toggle("hl", k === n));
    opts[n]?.scrollIntoView({ block: "nearest" });
  });
  $("#ld-path-list").addEventListener("mousedown", (e) => { const o = e.target.closest("[data-ld-dir]"); if (!o) return; e.preventDefault(); ldPathEnter(+o.dataset.ldDir); });
  $("#ld-path").addEventListener("blur", () => setTimeout(ldPathClose, 120));

  // ---------- magi review / propose (B1) ----------
  const MAGI_PRESET = { critic: (p) => ({ agent: "omp", model: "gpt-6-astra", name: `${p} Astra`, role: "적대적 검토자" }), proposer: (p) => ({ agent: "antigravity", model: "gemini-4-argon-medium", name: `${p} Argon`, role: "제안자" }) };
  // ---------- magi runs: phase + per-target progress (state.magiRuns, plus fresh POST responses) ----------
  const MAGI_PHASE_KO = { queued: "전달 대기", held: "보류", delivered: "전달됨", running: "진행 중", answered: "답 모두 도착", reporting: "종합 지시함", failed: "실패", cancelled: "취소됨" };
  const t_ = (x) => window.t ? window.t(x) : x;
  const MAGI_TSTATUS_KO = { ...STATUS_KO, not_asked: "아직 안 물음" };
  const MAGI_KIND_KO = { review: "검토", propose: "제안", both: "검토·제안" };
  const MAGI_HELD_KO = { ...HELD_KO, possible_draft: "입력창 초안" };
  const magiLocal = new Map(); // id → view from POST /api/magi, /deliver, /cancel (until /api/state carries it)
  function magiRuns() {
    const byId = new Map(magiLocal);
    for (const r of state?.magiRuns || []) {
      const l = magiLocal.get(r.id);
      // The state snapshot may be older than an action we just took; keep ours until the state catches up.
      if (l && l._at && (!state.generatedAt || state.generatedAt < l._at) && l.status !== r.status) continue;
      byId.set(r.id, r); magiLocal.delete(r.id);
    }
    return [...byId.values()].sort((a, b) => (b.createdAt || 0) - (a.createdAt || 0));
  }
  const magiActive = (r) => r.phase !== "cancelled" && !(r.phase === "reporting" && Date.now() - (r.synthSentAt || 0) > 30 * 60000);
  const magiRunsOf = (name) => magiRuns().filter((r) => r.source === name && magiActive(r));
  const magiCanDeliver = (r) => r.status === "queued" || r.status === "held";
  const magiCanCancel = (r) => r.status === "queued" || r.status === "held" || r.status === "failed";
  const magiPhaseCls = (p) => ({ held: "warn", failed: "bad", queued: "warn", answered: "ok", reporting: "ok" })[p] || "";
  function magiRunHtml(r, withSource) {
    const ts = (r.targets || []).map((t) => `<span class="mg-tg st-${esc(t.status)}" title="${esc(t.threadId || "")}">${esc(t.name)} <span class="muted">${esc(t_(MAGI_TSTATUS_KO[t.status] || t.status))}</span></span>`).join("");
    const held = r.status === "held" && r.heldReason ? `<span class="muted small">${esc(MAGI_HELD_KO[r.heldReason] || r.heldReason)} 때문에 보류</span>` : "";
    const err = r.error ? `<span class="small bad">${esc(r.error)}</span>` : "";
    const btns = `${magiCanDeliver(r) ? `<button data-act="magi-deliver" data-id="${esc(r.id)}" title="입력창 초안이 있어도 전달 (작업 중인 턴에는 넣지 않음)">그래도 전달</button>` : ""}${magiCanCancel(r) ? `<button data-act="magi-cancel" data-id="${esc(r.id)}">취소</button>` : ""}`;
    return `<div class="mg-run" data-run="${esc(r.id)}"><div class="mg-rh"><b>magi ${esc(MAGI_KIND_KO[r.kind] || r.kind || "")}</b>${withSource ? `<span>${esc(r.source)}</span>` : ""}<span class="muted small">${esc(r.mode || "")}${r.createdAt ? ` · ${ago(r.createdAt)}` : ""}</span><span class="mg-ph ${magiPhaseCls(r.phase)}">${esc(MAGI_PHASE_KO[r.phase] || r.phase)}</span>${held}${err}<span class="spacer"></span>${btns}</div><div class="mg-tgs">${ts}</div></div>`;
  }
  function renderMagiStrip() {
    const el = $("#magi-strip");
    const rs = ov.open && ov.name ? magiRunsOf(ov.name) : [];
    el.hidden = !rs.length;
    H("#magi-strip").innerHTML = rs.map((r) => magiRunHtml(r, false)).join("");
  }
  const magiSeen = (v) => { if (v?.id) { magiLocal.set(v.id, { ...v, _at: Date.now() }); render(); } };
  async function magiAction(id, what) {
    try {
      const v = await api("POST", `/api/magi/${enc(id)}/${what}`, {});
      magiSeen(v);
      toast(what === "cancel" ? "magi 요청을 취소했습니다" : v?.status === "delivered" ? "전달했습니다" : `아직 보류: ${MAGI_HELD_KO[v?.heldReason] || v?.heldReason || v?.status}`);
    } catch (e) { if (e.status === 409) { toast("이미 전달된 요청입니다"); if (!FIXTURE) refresh(); } }
  }
  async function openMagi(source, kind) {
    const s = agentBy(source);
    if (!s) return;
    let roles = { critic: [], proposer: [], missing: [] };
    try { roles = (await api("GET", `/api/magi/roles?source=${enc(source)}`, undefined, { quiet: true })) || roles; }
    catch (e) { toast(`역할 정보를 불러오지 못했습니다: ${e.message}`); }
    const wanted = [...(kind !== "propose" ? roles.critic || [] : []), ...(kind !== "review" ? roles.proposer || [] : [])];
    const defaults = new Set(wanted.filter((r) => r.default).map((r) => r.name));
    const roleOf = (n) => ((roles.critic || []).some((r) => r.name === n) ? "검토자 후보" : (roles.proposer || []).some((r) => r.name === n) ? "제안자 후보" : "");
    const live = state.agents.filter((a) => a.name !== source);
    const same = live.filter((a) => a.project === s.project), other = live.filter((a) => a.project !== s.project);
    const row = (a) => `<label class="mg-t"><input type="checkbox" name="t" value="${esc(a.name)}" ${defaults.has(a.name) ? "checked" : ""}><b>${esc(a.name)}</b><span class="muted small">${esc(TYPE_SHORT[a.agentType] || a.agentType)}${a.model ? ` · ${esc(a.model)}` : ""}${roleOf(a.name) ? ` · ${roleOf(a.name)}` : ""}</span></label>`;
    const missing = (roles.missing || []).filter((m) => kind === "both" || (kind === "review" ? m === "critic" : m === "proposer"));
    const title = kind === "review" ? "검토" : kind === "propose" ? "제안" : "검토·제안";
    dialog(`<h3>${title} 요청 · ${esc(source)}</h3>
      <div class="mg-sec"><div class="muted small">대상 (${esc(s.project)})</div>${same.map(row).join("") || '<p class="muted small">같은 프로젝트에 다른 에이전트가 없습니다</p>'}
        ${other.length ? `<details><summary class="muted small">다른 프로젝트 ${other.length}개</summary>${other.map(row).join("")}</details>` : ""}</div>
      ${missing.length ? `<div class="mg-missing">${missing.map((m) => `<button type="button" data-act="magi-launch" data-role="${m}" data-proj="${esc(s.project)}">${m === "critic" ? "Astra 띄우기" : "Argon 띄우기"}</button>`).join("")}<span class="muted small">${missing.map((m) => (m === "critic" ? "검토자" : "제안자")).join("·")} 역할의 에이전트가 없습니다</span></div>` : ""}
      <div class="row mg-mode">모드 <select name="mode">${kind === "propose" ? '<option value="propose">propose (선택지·시안 제안)</option>' : '<option value="accept">accept (적대적 수락 판정)</option><option value="discuss">discuss (논의)</option>'}</select></div>
      <input type="text" name="note" placeholder="(선택) 한 줄 메모">`, "보내기", (fd) => {
      const targets = fd.getAll("t");
      if (!targets.length) return toast("대상을 고르세요");
      const body = { source, kind, targets, mode: fd.get("mode") };
      const note = String(fd.get("note") || "").trim();
      if (note) body.note = note;
      api("POST", "/api/magi", body).then((v) => {
        magiSeen(v);
        toast(`${title} 요청 · ${MAGI_PHASE_KO[v?.phase] || v?.phase || "보냄"}${v?.status === "held" && v.heldReason ? ` (${MAGI_HELD_KO[v.heldReason] || v.heldReason})` : ""}`);
      }).catch(() => null);
    });
  }

  // ---------- slash menu (v16): "/" at the start of a line lists THIS agent's skills / commands ----------
  // GET /api/agents/{name}/commands → {harness, items:[{kind, name, insert, description, source, scope}]}.
  const cmdCache = new Map(); // agent → {items, at, err}
  const slash = { open: false, sel: 0, items: [], start: 0, end: 0, agent: null, mid: false };
  const SCOPE_KO = { project: "프로젝트", user: "사용자", builtin: "내장" };
  const KIND_KO = { recent: "최근", skill: "스킬", command: "명령" };
  const itemKey = (it) => `${it.kind}:${it.name}`;
  const recentKey = (agent) => `adash.cmdRecent.${agent}`;
  const recentOf = (agent) => { try { return JSON.parse(localStorage.getItem(recentKey(agent)) || "[]"); } catch { return []; } };
  async function loadCommands(agent) {
    const c = cmdCache.get(agent);
    if (c && Date.now() - c.at < 60000) return c.items;
    let items = [], err = null, prefixes = null;
    try { const r = await api("GET", `/api/agents/${enc(agent)}/commands`, undefined, { quiet: true }); items = r?.items || []; prefixes = Array.isArray(r?.prefixes) ? r.prefixes : null; }
    catch (e) { err = e.message; }
    items = items.filter((x) => x.kind !== "template"); // message templates were removed (v0.1.0)
    cmdCache.set(agent, { items, at: Date.now(), err, prefixes });
    return items;
  }
  // Ranking (v20): per query word, the best of
  //   exact name 1000 > name prefix 800 > word-initials (review-panel ← "rp", progress-report ← "pr") 700
  //   > a word of the name starts with it 600 > substring 450 > in-order subsequence ≤300
  //   > one-typo match against a name word (queries ≥ 4 chars) 200 > description substring (Korean too) 120.
  // Every query word must match somewhere; scores add up. `hits` = name indices to highlight.
  const NAME_SEP = /[-_:./\s]/;
  function wordsOf(n) { const out = []; let start = 0; for (let i = 0; i <= n.length; i++) if (i === n.length || NAME_SEP.test(n[i])) { if (i > start) out.push([start, n.slice(start, i)]); start = i + 1; } return out; }
  function oneTypo(a, b) { // Damerau distance ≤ 1
    if (Math.abs(a.length - b.length) > 1) return false;
    let i = 0; while (i < a.length && i < b.length && a[i] === b[i]) i++;
    if (i >= Math.min(a.length, b.length)) return true;
    return a.slice(i + 1) === b.slice(i + 1) || a.slice(i + 1) === b.slice(i) || a.slice(i) === b.slice(i + 1) || (a[i] === b[i + 1] && a[i + 1] === b[i] && a.slice(i + 2) === b.slice(i + 2));
  }
  function matchWord(t, n, d) {
    const range = (a, len) => Array.from({ length: len }, (_, k) => a + k);
    if (n === t) return { s: 1000, hits: range(0, n.length) };
    if (n.startsWith(t)) return { s: 800 - Math.min(99, n.length - t.length), hits: range(0, t.length) };
    const ws = wordsOf(n);
    if (t.length >= 2 && ws.length >= 2) {
      // initials, also with a word's first letters spilling over ("mres" → m + res)
      let wi = 0, ti = 0; const hits = [];
      while (wi < ws.length && ti < t.length) {
        const [st, w] = ws[wi]; let k = 0;
        while (k < w.length && ti < t.length && w[k] === t[ti]) { hits.push(st + k); k++; ti++; }
        if (!k) break;
        wi++;
      }
      if (ti === t.length && wi >= 2) return { s: 700 - (ws.length - wi), hits };
    }
    const w = ws.find(([, x]) => x.startsWith(t));
    if (w) return { s: 600 - w[0] / 10, hits: range(w[0], t.length) };
    const at = n.indexOf(t);
    if (at >= 0) return { s: 450 - at / 10, hits: range(at, t.length) };
    { let i = 0; const hits = []; for (let k = 0; k < n.length && i < t.length; k++) if (n[k] === t[i]) { hits.push(k); i++; }
      if (i === t.length && t.length >= 2) return { s: 300 - Math.min(99, hits[hits.length - 1] - hits[0] - t.length), hits }; }
    if (t.length >= 4) { const tw = ws.find(([, x]) => oneTypo(t, x.slice(0, Math.max(t.length, Math.min(x.length, t.length + 1)))) || oneTypo(t, x)); if (tw) return { s: 200, hits: range(tw[0], Math.min(tw[1].length, t.length)) }; }
    if (d.includes(t)) return { s: 120, hits: [] };
    return null;
  }
  function cmdMatch(q, it) {
    if (!q) return { s: 1, hits: [] };
    const n = it.name.toLowerCase(), d = String(it.description || "").toLowerCase();
    let s = 0; const hits = new Set();
    for (const t of q.split(/\s+/).filter(Boolean)) {
      const m = matchWord(t, n, d);
      if (!m) return null;
      s += m.s; m.hits.forEach((h) => hits.add(h));
    }
    return { s, hits: [...hits] };
  }
  const markHits = (name, hits) => { const hs = new Set(hits); return [...name].map((ch, i) => (hs.has(i) ? `<mark>${esc(ch)}</mark>` : esc(ch))).join(""); };
  // The "/token" under the caret: opens when the "/" starts the composer or follows whitespace / a newline / an
  // opening bracket; a "/" glued to text (~/a/b, _guide/x.md, http://) is a path and never opens the menu.
  // The query may hold spaces ("acad cv"); the menu then stays open only while something still matches.
  function slashToken(ta) {
    const pos = ta.selectionStart;
    if (ta.selectionEnd !== pos) return null;
    const before = ta.value.slice(0, pos);
    const m = /(^|[\s([{（「『<])\/([^\n/]*)$/.exec(before);
    if (!m || /\s\s|^\s/.test(m[2])) return null;
    const start = m.index + m[1].length, tail = /\s/.test(m[2]) ? "" : /^[^\s]*/.exec(ta.value.slice(pos))[0];
    const ls = before.lastIndexOf("\n") + 1;
    return { start, end: pos + tail.length, q: m[2].toLowerCase(), mid: start > ls };
  }
  // Caret position in viewport px, via a mirror element with the textarea's text metrics.
  function caretXY(ta, pos) {
    const cs = getComputedStyle(ta), m = document.createElement("div");
    for (const p of ["font", "letterSpacing", "lineHeight", "padding", "border", "boxSizing", "whiteSpace", "wordWrap", "width", "tabSize"]) m.style[p] = cs[p];
    Object.assign(m.style, { position: "fixed", visibility: "hidden", whiteSpace: "pre-wrap", wordWrap: "break-word", top: "0", left: "0", overflow: "hidden" });
    m.textContent = ta.value.slice(0, pos);
    const mark = document.createElement("span"); mark.textContent = "\u200b"; m.appendChild(mark);
    document.body.appendChild(m);
    const r = ta.getBoundingClientRect(), x = mark.offsetLeft, y = mark.offsetTop - ta.scrollTop;
    m.remove();
    return { x: r.left + x, y: r.top + y, lh: parseFloat(cs.lineHeight) || 18, r };
  }
  // ---------- '@' path completion (v25): GET /api/agents/{name}/paths?q= ----------
  // → {base, format, items: [{path, kind: "dir"|"file", size, mtime, insert}], ms}. `path` is relative to the
  // agent's cwd (dirs end with "/"); `insert` is the harness's own reference form ("@path", '@"a b"', or the bare
  // path for codex/agy) for files and folders alike. q may also start with "~/", "/" or "../".
  // Shares the slash list: same popup, keys, IME and phone placement. Opens on an "@" at the start / after
  // whitespace or an opening bracket; an "@" glued to text (a@b.com) is not a path. After a folder is picked the
  // token is tracked from where it was inserted (slash.pathCtx), since a bare-path insert has no "@" left.
  const pathQ = (seg) => seg.replace(/^@/, "").replace(/^"/, "").replace(/"$/, "");
  function atToken(ta) {
    const pos = ta.selectionStart;
    if (ta.selectionEnd !== pos) return null;
    const v = ta.value, ctx = slash.pathCtx;
    if (ctx && ctx.agent === ov.name && pos >= ctx.start) {
      const seg = v.slice(ctx.start, pos), quoted = /^@?"/.test(seg);
      if (!/\n/.test(seg) && (quoted ? !/"./.test(seg.replace(/^@?"/, "")) : !/\s/.test(seg))) {
        const tail = /^[^\s]*/.exec(v.slice(pos))[0];
        return { start: ctx.start, end: pos + tail.length, q: pathQ(seg) };
      }
    }
    slash.pathCtx = null;
    const m = /(^|[\s([{（「『<])@("[^"\n]*|[^\s@"]*)$/.exec(v.slice(0, pos));
    if (!m) return null;
    const start = m.index + m[1].length, tail = /^[^\s]*/.exec(v.slice(pos))[0];
    return { start, end: pos + tail.length, q: pathQ(`@${m[2]}`) };
  }
  const pathCache = new Map(); // `${agent}\n${q}` → {items, err}
  async function loadPaths(agent, q) {
    const k = `${agent}\n${q}`;
    if (pathCache.has(k)) return pathCache.get(k);
    let v;
    try { const r = await api("GET", `/api/agents/${enc(agent)}/paths?q=${encodeURIComponent(q)}`, undefined, { quiet: true }); v = { items: r?.items || [] }; }
    catch (e) { v = { items: [], err: e.status === 404 ? "이 서버는 경로 자동완성을 지원하지 않습니다" : e.message }; }
    pathCache.set(k, v);
    if (pathCache.size > 200) pathCache.delete(pathCache.keys().next().value);
    return v;
  }
  // Matched letters of the query in the path: a contiguous run if there is one (last occurrence, so the file
  // name wins over a folder), else the letters in order.
  function pathHits(path, q) {
    const p = path.toLowerCase(), t = q.toLowerCase();
    if (!t) return [];
    const at = p.lastIndexOf(t);
    if (at >= 0) return Array.from({ length: t.length }, (_, k) => at + k);
    const hits = []; let i = 0;
    for (let k = 0; k < p.length && i < t.length; k++) if (p[k] === t[i]) { hits.push(k); i++; }
    return i === t.length ? hits : [];
  }
  const ICON_DIR = `<svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path d="M1.5 3.5h4.2l1.5 1.5h7.3v8.5h-13z" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/></svg>`;
  const ICON_FILE = `<svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path d="M3.5 1.5h6l3 3v10h-9z M9.5 1.5v3h3" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/></svg>`;
  async function updateAt(ta, tok) {
    const pop = $("#slash-pop"), agent = ov.name;
    const seq = (slash.seq = (slash.seq || 0) + 1);
    const r = await loadPaths(agent, tok.q);
    const now2 = atToken(ta);
    if (seq !== slash.seq || !now2 || now2.q !== tok.q || ov.name !== agent) return;
    Object.assign(slash, { open: true, mode: "path", start: now2.start, end: now2.end, agent, mid: true });
    // Folders first, then files; the server's order inside each group.
    const items = [...r.items.filter((x) => x.kind === "dir"), ...r.items.filter((x) => x.kind !== "dir")];
    slash.items = items;
    slash.sel = Math.min(slash.sel, Math.max(0, items.length - 1));
    const meta = (x) => x.kind === "dir" ? "" : [x.size != null ? fmtBytes(x.size) : "", x.mtime ? ago(x.mtime) : ""].filter(Boolean).join(" · ");
    // Name first (bold, matched letters marked), then its parent folder dimmed. The directory prefix already typed
    // ("~/…/projects/") is not repeated: names are shown relative to it. A long parent is cut on the left by CSS
    // (rtl ellipsis) so the nearest folders stay visible. The full path is the row tooltip.
    const typedDir = tok.q.slice(0, tok.q.lastIndexOf("/") + 1), needle = tok.q.slice(typedDir.length);
    const split = (p) => {
      const rel = typedDir && p.startsWith(typedDir) ? p.slice(typedDir.length) : p;
      const body = rel.replace(/\/$/, ""), cut = body.lastIndexOf("/");
      return { name: body.slice(cut + 1) + (rel.endsWith("/") ? "/" : ""), parent: cut >= 0 ? body.slice(0, cut + 1) : "" };
    };
    pop.innerHTML = items.length ? items.map((x, i) => {
      const { name, parent } = split(x.path);
      return `<div class="sl-i path${i === slash.sel ? " on" : ""}" data-sl="${i}" title="${esc(x.path)}"><span class="sl-ic${x.kind === "dir" ? " dir" : ""}">${x.kind === "dir" ? ICON_DIR : ICON_FILE}</span><span class="sl-n">${markHits(name, pathHits(name, needle))}</span><span class="sl-par">${parent ? `<bdi>${esc(parent)}</bdi>` : ""}</span><span class="sl-d">${esc(meta(x))}</span></div>`;
    }).join("")
      : `<div class="sl-empty muted small">${r.err ? esc(r.err) : `'@${esc(tok.q)}'와 일치하는 파일·폴더 없음`}</div>`;
    placeSlash(ta, now2);
    pop.hidden = false;
    if (isMobile() && !slash.hist) { window.history.pushState({ adashSlash: 1 }, ""); slash.hist = true; }
    pop.querySelector(".sl-i.on")?.scrollIntoView({ block: "nearest" });
  }
  async function updateSlash() {
    const ta = $("#composer-text"), pop = $("#slash-pop");
    const at = ov.open && ov.name ? atToken(ta) : null;
    if (at) return updateAt(ta, at);
    slash.mode = "cmd";
    const tok = ov.open && ov.name ? slashToken(ta) : null;
    if (!tok) return closeSlash();
    const agent = ov.name;
    const all = await loadCommands(agent);
    // The composer may have changed while loading.
    const now2 = slashToken(ta);
    if (!now2 || ov.name !== agent) return closeSlash();
    Object.assign(slash, { open: true, start: now2.start, end: now2.end, agent, mid: now2.mid });
    const q = now2.q.trim(), rec = recentOf(agent);
    // An item already chosen ("/review-panel args…"): its arguments are being typed, not a search.
    if (/\s/.test(now2.q) && all.some((it) => it.name.toLowerCase() === q.split(/\s+/)[0] || String(it.insert || "").toLowerCase() === `/${q.split(/\s+/)[0]}`)) return closeSlash();
    // Empty query: a 최근 section on top. With a query: recency adds a small bonus, so a better match still wins.
    const scored = all.map((it) => { const m = cmdMatch(q, it); if (!m) return null; const r = rec.indexOf(itemKey(it)); return { it, s: m.s + (q && r >= 0 ? 40 - r : 0), hits: m.hits }; }).filter(Boolean);
    if (q && !scored.length && /\s/.test(now2.q)) return closeSlash();
    const recentItems = q ? [] : rec.map((k) => scored.find((x) => itemKey(x.it) === k)).filter(Boolean);
    const rest = scored.filter((x) => !recentItems.includes(x));
    const byScore = (a, b) => b.s - a.s || a.it.name.length - b.it.name.length || a.it.name.localeCompare(b.it.name);
    // With a query: one ranked list (best match first, whatever its kind); without: sections by kind.
    const sections = q ? [["match", rest.sort(byScore)]]
      : [["recent", recentItems.slice(0, 5)], ...["skill", "command"].map((k) => [k, rest.filter((x) => x.it.kind === k).sort(byScore)])].filter(([, xs]) => xs.length);
    slash.items = sections.flatMap(([, xs]) => xs.map((x) => x.it));
    slash.sel = Math.min(slash.sel, Math.max(0, slash.items.length - 1));
    let i = 0;
    const err = cmdCache.get(agent)?.err;
    const label = (it) => it.insert || `/${it.name}`;
    // Highlight the matched letters of the name inside its insert form ("/skill:" prefix kept plain).
    const labelHtml = (x) => { const L = label(x.it), at = L.toLowerCase().lastIndexOf(x.it.name.toLowerCase()); return at < 0 ? esc(L) : esc(L.slice(0, at)) + markHits(L.slice(at, at + x.it.name.length), x.hits) + esc(L.slice(at + x.it.name.length)); };
    pop.innerHTML = (slash.items.length ? sections.map(([k, xs]) => (k === "match" ? "" : `<div class="sl-sec">${KIND_KO[k]}</div>`) + xs.map((x) => {
      const it = x.it, idx = i++;
      return `<div class="sl-i${idx === slash.sel ? " on" : ""}" data-sl="${idx}" title="${esc(`${label(it)}${it.description ? `\n${String(it.description).split("\n")[0]}` : ""}`)}"><span class="sl-n">${labelHtml(x)}</span><span class="sl-d">${esc(String(it.description || "").split("\n")[0])}</span>${it.scope ? `<span class="sl-sc s-${esc(it.scope)}">${esc(k === "match" ? `${KIND_KO[it.kind] || it.kind} · ` : "")}${esc(SCOPE_KO[it.scope] || it.scope)}</span>` : ""}</div>`;
    }).join("")).join("") : `<div class="sl-empty muted small">'/${esc(q)}'와 일치하는 스킬·명령 없음</div>`) +
      (err ? `<div class="sl-foot muted small">이 에이전트의 스킬 목록을 불러오지 못함 (${esc(err)})</div>` : "");
    placeSlash(ta, now2);
    pop.hidden = false;
    // `history` is the timeline data in this module; the browser history API is window.history.
    if (isMobile() && !slash.hist) { window.history.pushState({ adashSlash: 1 }, ""); slash.hist = true; }
    pop.querySelector(".sl-i.on")?.scrollIntoView({ block: "nearest" });
  }
  // Desktop: anchored at the caret. Phone: a compact list glued to the top of the composer, sized to the part of
  // the screen the keyboard leaves visible (visualViewport): ≤ 40 % of it and ≤ ~6 rows, never covering the input.
  function placeSlash(ta, tok) {
    const pop = $("#slash-pop");
    pop.classList.remove("sheet");
    pop.classList.toggle("compact", isMobile());
    if (!isMobile()) {
      // As wide as the input (≥ 360 px), anchored at the caret but kept on screen.
      const c = caretXY(ta, tok.end), w = Math.min(window.innerWidth - 16, Math.max(360, c.r.width));
      Object.assign(pop.style, { width: `${w}px`, left: `${Math.max(8, Math.min(c.x - 8, window.innerWidth - w - 8))}px`, right: "", bottom: `${window.innerHeight - c.y + 4}px`, maxHeight: "" });
      return;
    }
    const vv = window.visualViewport, vTop = vv ? vv.offsetTop : 0, vH = vv ? vv.height : window.innerHeight;
    const edge = Math.min(ta.getBoundingClientRect().top, vTop + vH) - 4; // just above the input, inside the visible area
    const room = Math.max(88, edge - vTop - 8);
    Object.assign(pop.style, { left: "8px", right: "8px", width: "auto", bottom: `${window.innerHeight - edge}px`, maxHeight: `${Math.min(room, Math.round(vH * 0.4), 6 * 46 + 8)}px` });
  }
  window.visualViewport?.addEventListener("resize", () => { if (slash.open) placeSlash($("#composer-text"), { end: slash.end }); });
  window.visualViewport?.addEventListener("scroll", () => { if (slash.open) placeSlash($("#composer-text"), { end: slash.end }); });
  // Phone back button closes the list (one history entry while it is open).
  window.addEventListener("popstate", () => { if (slash.hist) { slash.hist = false; if (slash.open) { slash.open = false; slash.sel = 0; $("#slash-pop").hidden = true; } } });
  function closeSlash() {
    slash.open = false; slash.sel = 0; slash.pathCtx = null; $("#slash-pop").hidden = true;
    if (slash.hist) { slash.hist = false; window.history.back(); }
  }
  function pickSlash(i) {
    const it = slash.items[i], ta = $("#composer-text");
    if (!it) return;
    if (slash.mode === "path") {
      // Both kinds use the harness's `insert`. Folder: no trailing space, caret before a closing quote, list stays
      // open on q=path (go deeper). File: insert + a space, list closes.
      const form = it.insert || `@${it.path}`;
      const ins = it.kind === "dir" ? form : `${form}${/^\s/.test(ta.value.slice(slash.end)) ? "" : " "}`;
      ta.value = ta.value.slice(0, slash.start) + ins + ta.value.slice(slash.end);
      const pos = slash.start + ins.length - (it.kind === "dir" && form.endsWith('"') ? 1 : 0);
      ta.focus(); ta.setSelectionRange(pos, pos);
      if (it.kind === "dir") { slash.pathCtx = { start: slash.start, agent: slash.agent }; slash.sel = 0; return updateSlash(); }
      slash.pathCtx = null;
      return closeSlash();
    }
    // Mid-message, the harness may need a different form (catalog `midInsert`, e.g. a skill:// reference).
    const form = slash.mid && it.midInsert ? it.midInsert : it.insert || `/${it.name}`;
    const ins = /^\s/.test(ta.value.slice(slash.end)) ? form : `${form} `;
    ta.value = ta.value.slice(0, slash.start) + ins + ta.value.slice(slash.end);
    const pos = slash.start + ins.length;
    ta.focus(); ta.setSelectionRange(pos, pos);
    const rec = recentOf(slash.agent).filter((k) => k !== itemKey(it));
    localStorage.setItem(recentKey(slash.agent), JSON.stringify([itemKey(it), ...rec].slice(0, 12)));
    closeSlash();
  }
  function slashKey(e) {
    if (!slash.open || e.isComposing || e.keyCode === 229) return false;
    const n = slash.items.length;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); slash.sel = (slash.sel + (e.key === "ArrowDown" ? 1 : -1) + n) % Math.max(1, n); updateSlash(); return true; }
    if ((e.key === "Enter" || e.key === "Tab") && n) { e.preventDefault(); pickSlash(slash.sel); return true; }
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closeSlash(); return true; }
    return false;
  }

  // ---------- multi-select + broadcast (B2) ----------
  const bulk = new Set();
  let bulkAnchor = null;
  function toggleBulk(name, range) {
    const order = agentRows().map((r) => r.a.name);
    if (range && bulkAnchor && order.includes(bulkAnchor)) {
      const [i, j] = [order.indexOf(bulkAnchor), order.indexOf(name)].sort((p, q) => p - q);
      order.slice(i, j + 1).forEach((n) => bulk.add(n));
    } else { bulk.has(name) ? bulk.delete(name) : bulk.add(name); bulkAnchor = name; }
    render(); renderBulkBar();
  }
  // ---------- close / reopen agents (v24) ----------
  // POST /api/agents/{name}/close {mode, confirmWorking?, discardDraft?} → 409 {reason: working|asking|draft, draft?}
  // POST /api/close {targets, mode, confirmWorking?, discardDraft?} → {results:[{name, ok, reason?, error?}]}
  // POST /api/agents/{name}/reopen. The session is kept, so a closed agent can be reopened later.
  const CLOSE_WHY = { working: "작업 중", asking: "질문 대기", draft: "입력창 초안" };
  function closeWarnings(names) {
    return names.map((n) => {
      const s = agentBy(n), w = [];
      if (!s) return "";
      const k = kindOf(s);
      if (k === "working") w.push("작업 중입니다. 진행 중인 작업을 중단한 뒤 종료합니다.");
      if (k === "asking") w.push("답을 기다리는 질문이 있습니다. 답하지 않고 종료합니다.");
      if (s.draft?.text) w.push(`입력창에 보내지 않은 초안이 있습니다 (버려짐): <q>${esc(s.draft.text.split("\n")[0].slice(0, 120))}</q>`);
      return w.length ? `<li><b>${esc(n)}</b><ul>${w.map((x) => `<li>${x}</li>`).join("")}</ul></li>` : "";
    }).filter(Boolean);
  }
  function closeAgentsDlg(names, mode = "graceful") {
    if (!names.length) return;
    const warn = closeWarnings(names);
    const title = `${mode === "force" ? "강제 종료" : "종료"} · ${names.length === 1 ? esc(names[0]) : `${names.length}개 에이전트`}`;
    const what = mode === "force"
      ? "터미널을 바로 닫습니다(하네스가 정리할 시간 없음). 세션 기록은 남아 나중에 '다시 열기'로 이어 갈 수 있습니다."
      : "작업 중이면 먼저 중단하고, 하네스의 종료 명령으로 끝낸 뒤 터미널을 닫습니다. 세션 기록은 남아 나중에 '다시 열기'로 이어 갈 수 있습니다.";
    dialog(`<h3>${title}</h3><p>${what}</p>${names.length > 1 ? `<p class="muted small">${names.map(esc).join(", ")}</p>` : ""}${warn.length ? `<div class="close-warn"><b>주의</b><ul>${warn.join("")}</ul></div>` : ""}`, mode === "force" ? "강제 종료" : "종료", () => doClose(names, mode, true));
  }
  // 다시 시작: graceful close + reopen of the same session (the harness re-reads its sign-in).
  // Working / asking / a draft: the same warnings as 종료, then confirmed.
  function restartAgent(name) {
    // an open question: it is cancelled; offer to have the agent ask again afterwards
    if (agentBy(name)?.state === "asking") {
      dialog(`<h3>${t_("다시 시작")} · ${esc(name)}</h3><p>${esc(L2("질문이 취소됩니다.", "The open question is cancelled."))}</p><label class="ld-check"><input type="checkbox" id="rs-reask" checked> ${esc(L2("다시 시작 후 에이전트에게 같은 질문을 다시 물어보라고 알리기", "Tell the agent to ask again after the restart"))}</label>`, t_("다시 시작"), () => doRestart(name, true, $("#rs-reask")?.checked !== false));
      return;
    }
    const warn = closeWarnings([name]);
    if (!warn.length) return doRestart(name, false);
    dialog(`<h3>${t_("다시 시작")} · ${esc(name)}</h3><p>${t_("정상 종료한 뒤 같은 세션으로 다시 엽니다.")}</p><div class="close-warn"><b>${t_("주의")}</b><ul>${warn.join("")}</ul></div>`, t_("다시 시작"), () => doRestart(name, true));
  }
  async function doRestart(name, confirmed, reask) {
    const wasAuth = agentBy(name)?.state === "auth";
    toast(`${name} ${t_("다시 시작하는 중…")}`, 15000);
    try {
      await api("POST", `/api/agents/${enc(name)}/restart`, confirmed ? { confirmWorking: true, discardDraft: true, renotifyAsk: !!reask } : {}, { quiet: true });
      toast(`${name} ${t_("다시 시작함")}`, 3000);
    } catch (e) {
      toast(`${t_("다시 시작하지 않음:")} ${e.status === 409 ? CLOSE_WHY[e.reason] || e.message : e.message}`, 6000);
      return;
    }
    if (!wasAuth) return;
    // the login screen is checked about every 10 s: tell the user whether the sign-in took
    const t0 = Date.now();
    const check = async () => {
      const s = agentBy(name);
      if (s && s.state !== "auth" && s.handle) return toast(`${name}: ${t_("로그인 확인됨")}`, 5000);
      if (Date.now() - t0 > 40000) return toast(`${name}: ${t_("아직 로그인이 필요합니다. 터미널 보기에서 로그인하세요")}`, 8000);
      setTimeout(check, 5000);
    };
    setTimeout(check, 12000);
  }
  async function doClose(names, mode, confirmed) {
    const body = { mode, ...(confirmed ? { confirmWorking: true, discardDraft: true } : {}) };
    toast(`${names.length === 1 ? names[0] : `${names.length}개`} 종료하는 중…`, 10000);
    try {
      if (names.length === 1) {
        await api("POST", `/api/agents/${enc(names[0])}/close`, body, { quiet: true });
        toast(`${names[0]} 종료됨 · 세션은 남아 있음`, 3000);
        if (ov.open && ov.name === names[0]) closeChat();
      } else {
        const r = await api("POST", "/api/close", { targets: names, ...body }, { quiet: true });
        const res = r?.results || [];
        const ok = res.filter((x) => x.ok), bad = res.filter((x) => !x.ok);
        toast(`종료됨 ${ok.length} · 실패 ${bad.length}${bad.length ? `\n${bad.map((x) => `${x.name}: ${CLOSE_WHY[x.reason] || x.reason || x.error || "?"}`).join("\n")}` : ""}`, 6000);
        bulk.clear(); renderBulkBar();
      }
    } catch (e) {
      // The state changed after the dialog (e.g. a draft appeared): show the server's reason and ask again.
      if (e.status === 409) toast(`종료하지 않음: ${CLOSE_WHY[e.reason] || e.message}`, 5000);
      else toast(`종료 실패: ${e.message}`, 5000);
    }
    if (!FIXTURE) refresh(); else render();
  }
  async function reopenAgent(name) {
    toast(`${name} 다시 여는 중…`, 10000);
    try {
      await api("POST", `/api/agents/${enc(name)}/reopen`, {}, { quiet: true });
      toast(`${name} 다시 열림`, 3000);
    } catch (e) {
      toast(e.status === 409 ? `다시 열지 않음: 이미 같은 세션을 쓰는 프로세스가 있습니다` : `다시 열기 실패: ${e.message}`, 5000);
    }
    if (!FIXTURE) refresh(); else render();
  }
  function renderBulkBar() {
    const bar = $("#bulk-bar");
    document.body.classList.toggle("bulk-on", bulk.size > 0);
    if (!bulk.size) { bar.hidden = true; return; }
    bar.hidden = false;
    bar.innerHTML = `<b>${bulk.size}개 선택</b><span class="muted small">${[...bulk].map(esc).join(", ")}</span><span class="spacer"></span>
      <button data-act="bulk-msg">메시지 보내기</button><button data-act="bulk-ack">확인함</button>
      <button data-act="bulk-snooze" data-min="60">${L2("미루기 1시간", "Snooze 1 h")}</button><div class="menu-wrap"><button data-pop="pop-bulk-snz" title="${esc(L2("다른 시간으로 미루기", "Snooze for another time"))}">▾</button><div id="pop-bulk-snz" class="pop" hidden>${snzMenuHtml("bulk-snooze")}</div></div><button data-act="bulk-close" class="danger">종료…</button><button data-act="bulk-clear">선택 해제</button>`;
  }
  async function broadcast(body) {
    const r = await api("POST", "/api/broadcast", { targets: [...bulk], force: false, ...body }).catch(() => null);
    if (!r) return;
    const res = r.results || [];
    const held = res.filter((x) => !x.delivered);
    dialog(`<h3>보내기 결과</h3><ul class="bc-res">${res.map((x) => `<li class="${x.delivered ? "" : "held"}"><b>${esc(x.name)}</b> ${x.delivered ? `전달됨${x.mode ? ` · ${esc(MODE_KO[x.mode] || x.mode)}` : ""}` : `보류 · ${esc(x.held === "working" ? "작업 중" : x.held === "possible_draft" ? "입력창에 초안" : x.held || x.reason || "?")}`}</li>`).join("")}</ul>
      ${held.length ? `<p class="muted small">보류된 ${held.length}개는 자동으로 강제 전송하지 않습니다. 각 채팅에서 확인하세요.</p>` : ""}`, "닫기", () => {});
    if (!FIXTURE) refresh();
  }
  async function bulkAction(kind, b) {
    if (kind === "clear") { bulk.clear(); render(); return renderBulkBar(); }
    if (kind === "close") return closeAgentsDlg([...bulk]);
    if (kind === "ack" || kind === "snooze") {
      for (const n of bulk) await api("POST", `/api/agents/${enc(n)}/${kind}`, kind === "snooze" ? { minutes: +b.dataset.min } : undefined).catch(() => null);
      toast(`${bulk.size}개 ${kind === "ack" ? "확인함" : "미룸"}`, 2500);
      bulk.clear(); renderBulkBar(); if (!FIXTURE) refresh(); else render();
      return;
    }
    if (kind === "msg") return dialog(`<h3>${bulk.size}개 에이전트에게 메시지</h3><textarea name="text" required></textarea>`, "보내기", (fd) => { const text = String(fd.get("text")).trim(); if (text) broadcast({ text }); });
  }

  // ---------- command palette + full-text search (B3) ----------
  const pal = { mode: "jump", items: [], sel: 0, seq: 0 };
  const fuzzy = (q, s) => { q = q.toLowerCase(); s = String(s).toLowerCase(); if (!q) return 1; const i = s.indexOf(q); if (i >= 0) return 100 - i; let k = 0; for (const c of s) if (c === q[k]) k++; return k === q.length ? 10 : 0; };
  function palActions() {
    const cur = ov.open ? ov.name : selName;
    return [
      { label: L2("새 세션", "New session"), hint: "n", run: () => openLaunch() },
      { label: L2("지난 세션", "Past sessions"), hint: L2("찾아서 이어서 열기", "find and resume"), run: () => openSessions() },
      cur && { label: `${L2("검토 요청", "Request review")} · ${cur}`, run: () => openMagi(cur, "review") },
      cur && { label: `${L2("제안 요청", "Request proposal")} · ${cur}`, run: () => openMagi(cur, "propose") },
      bulk.size && { label: L2(`메시지 보내기 · ${bulk.size}개`, `Send message · ${bulk.size} agents`), run: () => bulkAction("msg") },
      { label: L2("보고서 열기", "Open reports"), run: () => (typeof openReports === "function" ? openReports() : toast(L2("보고서는 아직 없습니다", "No reports yet"))) },
      { label: L2("알림·기기 설정", "Notifications and devices"), run: () => openPushSettings() },
      ...(cur ? SNZ_MIN.map((m) => ({ label: `${L2("미루기", "Snooze")} ${snzLabel(m)} · ${cur}`, run: () => act("POST", `/api/agents/${enc(cur)}/snooze`, { minutes: m }, L2(`${snzLabel(m)} 미룸`, `snoozed ${snzLabel(m)}`)) })) : []),
      cur && { label: `${L2("미루기 직접 입력…", "Snooze custom…")} · ${cur}`, run: () => snoozeCustomDlg(cur) },
      { label: L2("확인함 모두", "Mark all read"), hint: L2("미확인 전부", "every unread"), run: async () => { const ns = model.agents.filter((a) => a.unread).map((a) => a.name); for (const n of ns) await api("POST", `/api/agents/${enc(n)}/ack`).catch(() => null); toast(L2(`${ns.length}개 확인함`, `${ns.length} marked read`)); if (!FIXTURE) refresh(); } },
    ].filter(Boolean);
  }
  function palJumpItems(q) {
    const items = [
      ...palActions().map((a) => ({ kind: "명령", label: a.label, sub: a.hint || "", run: a.run })),
      ...model.agents.filter((a) => a.s).map((a) => ({ kind: "에이전트", label: a.name, sub: `${a.project} · ${SEG_KO[a.kind] || a.kind}${a.role ? ` · ${a.role}` : ""}`, run: () => openChat(a.name) })),
      ...model.groups.flatMap((g) => [
        { kind: "프로젝트", label: `${t_("숨기기")} · ${g.name}`, sub: "계속 숨김 · 에이전트는 계속 작업", run: () => hideProject(g.name, "") },
        { kind: "프로젝트", label: `${t_("모두 종료")} · ${g.name}`, sub: `에이전트 ${g.allCount}`, run: () => closeProjectDlg(g.name) },
        { kind: "프로젝트", label: `${t_("보관")} · ${g.name}`, sub: "모두 종료하고 숨김", run: () => closeProjectDlg(g.name, true) }]),
      ...hiddenProjects.map((h) => ({ kind: "프로젝트", label: `${t_(h.archived ? "복원" : "보이기")} · ${h.name}`, sub: untilTxt(h), run: () => (h.archived ? projCall(h.name, "restore", {}, `${h.name} 복원`) : unhideProject(h.name)) })),
      ...model.groups.map((g) => ({ kind: "프로젝트", label: g.name, sub: `에이전트 ${g.allCount}${g.unread ? ` · 미확인 ${g.unread}` : ""}`, run: () => { closeChat?.(); jumpToProject(g.name); } })),
    ];
    return items.map((it) => ({ ...it, score: Math.max(fuzzy(q, it.label), fuzzy(q, it.sub) / 2) })).filter((it) => it.score > 0).sort((a, b) => b.score - a.score).slice(0, 40);
  }
  function snippetHtml(s) { return esc(s).replace(/&lt;(\/?)(mark|b|em)&gt;/g, "<$1mark>"); }
  async function palSearch(q) {
    const seq = ++pal.seq;
    if (!q.trim()) { pal.items = []; return renderPal(); }
    try {
      // sessions by title / name / project / first prompt first, then message hits
      const [ss, r] = await Promise.all([
        api("GET", `/api/sessions/search?q=${enc(q)}&limit=15`, undefined, { quiet: true }).catch(() => null),
        api("GET", `/api/search?q=${enc(q)}&limit=50`, undefined, { quiet: true }),
      ]);
      if (seq !== pal.seq) return;
      const when = (t) => (t ? new Date(t).toLocaleString("ko-KR") : "");
      const sessions = (ss?.sessions || []).filter((h) => inScope(h.project)).map((h) => ({ kind: "세션", label: sessLabel(h), sub: `${h.project || ""} · ${h.agentType} · ${when(h.lastTs)}${h.live ? "" : " · 종료됨"}`, snippet: DEMO.mask || !h.prompt || h.prompt === h.title ? "" : h.prompt, run: () => openHit(h) }));
      // Demo/project filter: only hits inside the shown projects; masked mode drops the message snippet.
      const msgs = (r?.hits || []).filter((h) => inScope(h.project)).map((h) => ({ kind: h.role === "user" ? "나" : h.role === "assistant" ? "답" : h.role, label: `${h.agent || h.session}`, sub: `${h.project || ""} · ${when(h.ts)}${h.live ? "" : " · 종료됨"}`, snippet: DEMO.mask ? "" : h.snippet, run: () => openHit(h) }));
      pal.items = [...sessions, ...msgs];
    } catch (e) { if (seq === pal.seq) pal.items = [{ kind: "오류", label: `검색 실패: ${e.message}`, sub: "", run: () => {} }]; }
    pal.sel = 0; renderPal();
  }
  function renderPal() {
    $$("[data-pal-mode]").forEach((b) => b.classList.toggle("on", b.dataset.palMode === pal.mode));
    $("#pal-list").innerHTML = pal.items.length ? pal.items.map((it, i) => `<div class="pal-i${i === pal.sel ? " on" : ""}" data-pal="${i}"><span class="pal-k">${esc(it.kind)}</span><span class="pal-l"><b>${esc(it.label)}</b><span class="muted small">${esc(it.sub)}</span>${it.snippet ? `<span class="pal-s">${snippetHtml(it.snippet)}</span>` : ""}</span></div>`).join("") : `<div class="muted small pal-empty">${pal.mode === "search" ? "검색어를 입력하세요 (세션 제목·이름·대화 내용)" : "일치하는 항목 없음"}</div>`;
    $(".pal-i.on")?.scrollIntoView({ block: "nearest" });
  }
  let palTimer = null;
  function palInput() {
    const v = $("#pal-input").value;
    if (v.startsWith("?") && pal.mode !== "search") pal.mode = "search";
    if (pal.mode === "search") { clearTimeout(palTimer); palTimer = setTimeout(() => palSearch(v.replace(/^\?/, "")), 180); renderPal(); }
    else { pal.items = palJumpItems(v); pal.sel = 0; renderPal(); }
  }
  function openPalette() { $("#palette").hidden = false; pal.mode = "jump"; $("#pal-input").value = ""; palInput(); $("#pal-input").focus(); }
  function closePalette() { $("#palette").hidden = true; }
  function palRun(i) { const it = pal.items[i]; if (!it) return; closePalette(); it.run(); }
  function palKey(e) {
    if (e.key === "Escape") { e.preventDefault(); return closePalette(); }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); const n = pal.items.length || 1; pal.sel = (pal.sel + (e.key === "ArrowDown" ? 1 : -1) + n) % n; return renderPal(); }
    if (e.key === "Enter") { e.preventDefault(); return palRun(pal.sel); }
    if (e.key === "Tab") { e.preventDefault(); pal.mode = pal.mode === "jump" ? "search" : "jump"; return palInput(); }
  }
  // A search hit: live agent → its chat scrolled to the message; ended session → read-only transcript
  // with "이어서 열기" (resume it in a new terminal). Session hits carry no messageId: show the end.
  async function openHit(h) {
    if (h.live && h.agent && agentBy(h.agent)) {
      openChat(h.agent);
      if (!h.messageId) return; // a session hit: the chat opens at its latest messages
      try {
        const r = await api("GET", `/api/agents/${enc(h.agent)}/transcript?around=${enc(h.messageId)}&limit=200`, undefined, { quiet: true });
        if (ov.name !== h.agent) return;
        if (r?.messages?.length && !r.messages.some((m) => ov.tx?.messages?.some((x) => x.id === m.id))) ov.tx = { ...r, messages: r.messages };
        renderChat(true);
      } catch { /* the latest page is shown instead */ }
      requestAnimationFrame(() => { const el = [...$("#chat-log").children].find((c) => c._k === h.messageId); if (el) { el.scrollIntoView({ block: "center" }); el.classList.add("hit"); setTimeout(() => el.classList.remove("hit"), 2000); } });
      return;
    }
    openViewer(`<p class="muted">불러오는 중…</p>`, true);
    try {
      const r = await api("GET", `/api/sessions/${enc(h.session)}/transcript`, undefined, { quiet: true });
      const body = (r?.messages || []).filter((m) => m.role !== "tool").map((m) => `<div class="ro-m ${esc(m.role)}${m.id === h.messageId ? " hit" : ""}" id="ro-${esc(m.id)}"><div class="muted small">${m.role === "user" ? "나" : esc(m.role)} · ${m.ts ? new Date(m.ts).toLocaleString("ko-KR") : ""}</div><div class="md">${md(m.text, null, m.id)}</div></div>`).join("");
      const label = h.agent || h.title || String(h.session);
      const resume = h.file && !FIXTURE ? `<button class="primary resume-btn" data-reopen-file="${esc(h.file)}" data-label="${esc(label)}" title="이 세션을 새 터미널에서 이어서 엶">이어서 열기</button>` : "";
      const back = h.fromList ? `<button data-sess-back title="지난 세션 목록으로">← 목록</button> ` : "";
      openViewer(`<div class="doc-path">${back}<b>${esc(label)}</b> <span class="muted small">읽기 전용 · ${esc(h.project || "")}</span> ${resume}</div>${body || '<p class="muted">메시지 없음</p>'}`, true);
      enhanceRich($("#viewer"));
      const target = h.messageId ? document.getElementById(`ro-${h.messageId}`) : [...$("#viewer .viewer-body").querySelectorAll(".ro-m")].pop();
      target?.scrollIntoView({ block: h.messageId ? "center" : "end" });
    } catch (e) { openViewer(`<p>대화를 열 수 없음: ${esc(e.message)}</p>`, true); }
  }
  async function reopenSession(file, label) {
    toast(`${label} 이어서 여는 중…`, 10000);
    try {
      const r = await api("POST", "/api/agents/_/reopen", { identity: file }, { quiet: true });
      toast(`${r?.name || label} 열림`, 3000);
      closeViewer();
    } catch (e) {
      toast(e.status === 409 ? `이어서 열지 않음: 이미 같은 세션을 쓰는 프로세스가 있습니다` : `이어서 열기 실패: ${e.message}`, 5000);
    }
    refresh();
  }
  // ---------- legend (v40): what the bars and marks mean ----------
  function legendHtml() {
    const bar = (cls, h, extra = "") => `<svg width="64" height="14" viewBox="0 0 64 14" aria-hidden="true"><rect x="2" y="${(14 - h) / 2}" width="60" height="${h}" rx="${Math.min(2.5, h / 2)}" class="${cls}" ${extra}/></svg>`;
    const rows = [
      [bar("lg-ask", 10), "질문 대기 · 내 답이 필요 (ASK)"],
      [bar("lg-err", 10), "오류로 멈춤 · 다시 시도 필요 (ERR)"],
      [bar("lg-wait", 8), "답을 보냄 · 내 차례 (TURN)"],
      [bar("lg-work", 5.5), "작업 중 · 지켜보기만"],
      [bar("lg-snz", 4), "미룸"],
      [bar("lg-idle", 2), "유휴 · 이전 대기"],
    ];
    const marks = [
      ["◷ 1:32", "캐시 만료 예상까지 남은 시간"], ["≡3", "정하실 것 (결정 항목 수)"], ["⏸", "답변 보류 (입력창 초안 등)"], ["⊶ 1/2", "magi 검토 진행"],
      ["✎", "터미널 입력창에 보내지 않은 초안"], ["2/6", "작업 단계 진행률"], ["+1", "접힌 보조 표시 (마우스를 올리면 전부)"], ["★4.0", "프로젝트 우선순위 점수"], ["NAME?", "이름을 자동으로 추정함"],
    ];
    return `<div><h4>타임라인 막대</h4>${rows.map(([s, t]) => `<div class="lg-row"><span>${s}</span><span>${t}</span></div>`).join("")}</div>
      <div><h4>기호</h4>${marks.map(([s, t]) => `<div class="lg-row"><span class="lg-sym">${s}</span><span>${t}</span></div>`).join("")}</div>`;
  }
  $("#legend-pop").innerHTML = legendHtml();
  document.addEventListener("keydown", (e) => {
    if (e.key !== "?" || e.ctrlKey || e.metaKey || e.altKey || isEditable(e.target) || ov.open) return;
    e.preventDefault();
    $("[data-pop=legend-pop]").click();
  });

  // ---------- themes (v40): built-in + user TOML themes ----------
  // GET /api/themes → {version, dir, themes: [{name, source, path, base, dark, warnings, errors}], tokens}
  // GET /api/themes/{name}.css → ':root{--…}' (contract variables only). Per device: a light theme, a dark theme and
  // whether to follow the system. /api/state.themeVersion changes when a theme file is saved → reload and preview.
  // Defaults (v42): Catppuccin Latte when light, Dracula when dark; a saved choice wins. Unknown names fall back
  // to the built-in adash light / dark when the server does not list them (older server).
  const themeCfg = (() => { try { return { light: "catppuccin-latte", dark: "dark", mode: "system", ...JSON.parse(localStorage.getItem("adash.theme") || "{}") }; } catch { return { light: "catppuccin-latte", dark: "dark", mode: "system" }; } })();
  let themeList = [{ name: "light", source: "builtin", dark: false }, { name: "dark", source: "builtin", dark: true }], themeDir = "", themeVer = null, themeRefused = [];
  const sysDark = window.matchMedia("(prefers-color-scheme: dark)");
  const wantDark = () => (themeCfg.mode === "dark" || (themeCfg.mode === "system" && sysDark.matches));
  const activeTheme = () => { const n = wantDark() ? themeCfg.dark : themeCfg.light; return themeList.some((t) => t.name === n) ? n : wantDark() ? "dark" : "light"; };
  // family / pair / swatch come from /api/themes (v42); older servers: inferred from the name.
  const FAMILY_ORDER = ["adash", "okabe-ito", "dracula", "catppuccin", "tokyo-night", "nord", "gruvbox", "solarized", "rose-pine", "kanagawa", "everforest", "github", "modus", "one", "ayu", "flexoki", "nightfox"];
  const FAMILY_LABEL = { adash: "adash", "okabe-ito": "Okabe–Ito (색약 안전)", dracula: "Dracula", catppuccin: "Catppuccin", "tokyo-night": "Tokyo Night", nord: "Nord", gruvbox: "Gruvbox", solarized: "Solarized", "rose-pine": "Rosé Pine", kanagawa: "Kanagawa", everforest: "Everforest", github: "GitHub", modus: "Modus", one: "One (Atom)", ayu: "Ayu", flexoki: "Flexoki", nightfox: "Nightfox", user: "내 테마" };
  const familyOf = (t) => t.family || (t.source === "user" ? "user" : /^dracula/.test(t.name) ? "dracula" : /^catppuccin/.test(t.name) ? "catppuccin" : /^tokyo-night/.test(t.name) ? "tokyo-night" : /^nord/.test(t.name) ? "nord" : /^gruvbox/.test(t.name) ? "gruvbox" : /^solarized/.test(t.name) ? "solarized" : "adash");
  const swatchHtml = (t) => (t?.swatch?.length ? `<span class="th-sw" aria-hidden="true">${t.swatch.map((c) => `<i style="background:${/^#[0-9a-f]{3,8}$/i.test(c) ? c : "transparent"}"></i>`).join("")}</span>` : "");
  async function applyTheme() {
    const name = activeTheme(), t = themeList.find((x) => x.name === name);
    document.documentElement.dataset.scheme = (t ? t.dark : wantDark()) ? "dark" : "light";
    document.documentElement.dataset.theme = name;
    // Every theme, light and dark included, comes from the server (the single definition); style.css keeps the
    // same light/dark values only as the fallback before JS runs or when the request fails.
    let css = "";
    try { const r = await fetch(FIXTURE ? `fixture_theme_${name}.css` : `/api/themes/${encodeURIComponent(name)}.css`, { cache: "no-store" }); if (r.ok) css = await r.text(); } catch { /* fallback: style.css */ }
    // One declaration per line ("  --name: value;"); anything else, or a value with unbalanced quotes, is dropped.
    const decl = css.split("\n").map((l) => /^\s*(--[a-z][a-z0-9-]*):\s*([#\p{L}\p{N} ,._"-]+);\s*$/u.exec(l)).filter((m) => m && (m[2].split('"').length - 1) % 2 === 0).map((m) => `  ${m[1]}: ${m[2]};`);
    $("#theme-css").textContent = decl.length ? `:root:root {\n${decl.join("\n")}\n}` : "";
    requestAnimationFrame(() => { if (typeof measureTkSlot === "function") measureTkSlot(); });
    document.body.classList.toggle("dense", getComputedStyle(document.documentElement).getPropertyValue("--density").trim() === "compact");
    // Files the server refused (built-in name, bad name, duplicate) are listed too, so a theme that never shows up is explained.
    const issues = [...themeList.filter((x) => [themeCfg.light, themeCfg.dark].includes(x.name)).flatMap((x) => [...(x.errors || []).map((m) => `${x.name} · 오류: ${m}`), ...(x.warnings || []).map((m) => `${x.name} · 경고: ${m}`)]), ...themeRefused.map((x) => `${String(x.path || "").split("/").pop()} · 불러오지 않음: ${x.reason || "?"}`)];
    const w = $("#fp-theme-warn"); w.hidden = !issues.length; w.innerHTML = issues.map((x) => `<span>${esc(x)}</span>`).join("") + (themeList.find((x) => x.name === name)?.errors?.length ? `<span class="muted small">오류가 있는 값은 기본 테마 값으로 대신합니다</span>` : "");
    $("meta[name=theme-color]")?.setAttribute("content", getComputedStyle(document.documentElement).getPropertyValue("--ui-surface").trim() || "#ffffff");
    if (typeof render === "function" && state) render();
  }
  async function loadThemes() {
    try {
      // Demo: the theme list is a snapshot of the server's GET /api/themes (fixture_themes.json) when present.
      const fx = FIXTURE ? await fetch("fixture_themes.json").then((x) => (x.ok ? x.json() : null)).catch(() => null) : null;
      const r = fx || FIXTURE ? fx || { version: 1, dir: "~/.config/adash/themes", themes: [{ name: "light", source: "builtin", dark: false }, { name: "dark", source: "builtin", dark: true }, { name: "high-contrast", source: "builtin", dark: false }, { name: "ink", source: "builtin", dark: true }, { name: "solarized-ish", source: "user", base: "dark", dark: true, path: "~/.config/adash/themes/solarized-ish.toml", warnings: ["signal.turn-text on signal.turn-bg: contrast 3.80:1 (text needs 4.5:1)"], errors: [] }], refused: [{ path: "~/.config/adash/themes/dark.toml", reason: "name \"dark\" is a built-in theme" }] }
        : await api("GET", "/api/themes", undefined, { quiet: true });
      if (r?.themes?.length) themeList = r.themes;
      // No saved choice on this device: the server's defaults ([theme] in config; adash dark / catppuccin-latte).
      if (!localStorage.getItem("adash.theme") && r?.defaults) { if (r.defaults.light) themeCfg.light = r.defaults.light; if (r.defaults.dark) themeCfg.dark = r.defaults.dark; }
      if (r?.dir) themeDir = r.dir;
      themeRefused = Array.isArray(r?.refused) ? r.refused : [];
    } catch { /* older server: built-ins only */ }
    // Grouped by family (optgroup), dark themes in the dark list and light ones in the light list.
    fillThemeSelects();
    $("#fp-theme-mode").value = themeCfg.mode;
    renderThemeSwatches();
    // Command/path samples are code (never translated by the i18n layer): pick the placeholder word per language.
    const nm = window.ADASH_LANG === "en" ? "name" : "이름";
    $("#fp-theme-dir").textContent = `${themeDir || "~/.config/adash/themes"}/<${nm}>.toml`;
    $("#fp-theme-new").textContent = `adash theme new ${nm} --from dark`;
    await applyTheme();
  }
  function fillThemeSelects() {
    // A search box narrows both lists (name, label or family); the current choice always stays listed.
    const q = ($("#fp-theme-q")?.value || "").trim().toLowerCase();
    const hit = (t) => !q || [t.name, t.label, familyOf(t), FAMILY_LABEL[familyOf(t)]].some((x) => String(x || "").toLowerCase().includes(q));
    const opt = (want, dark) => {
      const ts = themeList.filter((t) => (t.dark === dark && hit(t)) || t.name === want);
      return FAMILY_ORDER.concat([...new Set(ts.map(familyOf))].filter((f) => !FAMILY_ORDER.includes(f) && f !== "user"), ["user"]).map((f) => {
        const g = ts.filter((t) => familyOf(t) === f);
        return g.length ? `<optgroup label="${esc(t_(FAMILY_LABEL[f] || f))}">${g.map((t) => `<option value="${esc(t.name)}"${t.name === want ? " selected" : ""}>${esc(t.label || t.name)}${t.source === "user" ? t_(" (내 테마)") : ""}${t.errors?.length ? t_(" (오류)") : ""}</option>`).join("")}</optgroup>` : "";
      }).join("");
    };
    $("#fp-theme-light").innerHTML = opt(themeCfg.light, false);
    $("#fp-theme-dark").innerHTML = opt(themeCfg.dark, true);
  }
  const saveTheme = () => { localStorage.setItem("adash.theme", JSON.stringify(themeCfg)); applyTheme(); };
  // Picking one side suggests its family counterpart for the other side (pair), e.g. Gruvbox Dark ↔ Gruvbox Light.
  function pickTheme(side, name) {
    themeCfg[side] = name;
    const t = themeList.find((x) => x.name === name), other = side === "light" ? "dark" : "light";
    const pair = t?.pair && themeList.find((x) => x.name === t.pair);
    if (pair && familyOf(themeList.find((x) => x.name === themeCfg[other]) || {}) !== familyOf(t)) {
      themeCfg[other] = pair.name;
      $(`#fp-theme-${other}`).value = pair.name;
      toast(L2(`${other === "dark" ? "어두울 때" : "밝을 때"}도 같은 계열(${pair.label || pair.name})로 맞췄습니다`, `${other === "dark" ? "Dark" : "Light"} theme matched: ${pair.label || pair.name}`), 3000);
    }
    renderThemeSwatches();
    saveTheme();
  }
  function renderThemeSwatches() {
    for (const side of ["light", "dark"]) { const el = $(`#fp-theme-${side}-sw`); if (el) el.innerHTML = swatchHtml(themeList.find((x) => x.name === themeCfg[side])); }
  }
  $("#fp-theme-light").addEventListener("change", (e) => pickTheme("light", e.target.value));
  $("#fp-theme-dark").addEventListener("change", (e) => pickTheme("dark", e.target.value));
  $("#fp-theme-mode").addEventListener("change", (e) => { themeCfg.mode = e.target.value; saveTheme(); });
  sysDark.addEventListener("change", applyTheme);
  $("#fp-theme-q")?.addEventListener("input", () => fillThemeSelects());
  // Called from setState: a saved theme file bumps themeVersion → reload the list and the active CSS.
  function themeVersionSeen(v) { if (v == null) return; if (themeVer != null && v !== themeVer) loadThemes(); themeVer = v; }
  loadThemes();

  // ---------- project hide / close all / archive (v32) ----------
  // POST /api/projects/{p}/hide {until} · /unhide · /close {mode, confirm:[names]} · /archive · /restore · /reopen-all
  const L2 = (ko, en) => (window.ADASH_LANG === "en" ? en : ko);
  const projAgents = (p) => (state.agents || []).filter((a) => a.project === p);
  const untilTxt = (h) => (h.archived ? "보관됨" : h.until == null ? "계속 숨김" : `${fmtMin((h.until - Date.now()) / 60000) || "1분 미만"} 남음`);
  function projMenuHtml(p) {
    const n = projAgents(p).length;
    return `<div class="pm-head"><b>${esc(p)}</b><span class="muted small">에이전트 ${n}</span></div>
      <div class="pm-sec muted small">숨기기 (에이전트는 계속 작업)</div>
      <button data-pact="hide" data-until="1h" data-proj="${esc(p)}">1시간</button><button data-pact="hide" data-until="today" data-proj="${esc(p)}">오늘</button><button data-pact="hide" data-until="" data-proj="${esc(p)}">계속</button>
      <div class="pm-sep"></div>
      <button data-pact="close" data-proj="${esc(p)}"${n ? "" : " disabled"}><b>모두 종료…</b><span>${L2(`살아 있는 에이전트 ${n}개를 종료 · 세션은 남음`, `Close ${n} live agent${n === 1 ? "" : "s"} · sessions are kept`)}</span></button>
      <button data-pact="archive" data-proj="${esc(p)}"><b>보관…</b><span>모두 종료하고 계속 숨김 · 지난 세션에서 복원</span></button>`;
  }
  function openProjMenu(btn) {
    let m = $("#proj-menu");
    if (!m) { m = document.createElement("div"); m.id = "proj-menu"; m.className = "pop proj-menu"; document.body.appendChild(m); }
    m.innerHTML = projMenuHtml(btn.dataset.projMenu);
    m.hidden = false;
    placePop(m, btn, { align: "right" });
  }
  async function projCall(p, what, body, ok) {
    try {
      const r = await api("POST", `/api/projects/${enc(p)}/${what}`, body || {}, { quiet: true });
      if (ok) toast(typeof ok === "function" ? ok(r) : ok, 4000);
      return r;
    } catch (e) {
      // 409 {reason: "confirm", needs: [{name, why, draft?}]}: the state changed since the dialog; ask again with this list.
      if (e.status === 409 && e.data?.reason === "confirm") { closeProjectDlg(p, what === "archive", e.data.needs || []); return null; }
      toast(`${p}: ${e.message}`, 5000); return null;
    }
    finally { if (!FIXTURE) refresh(); else { await refresh(); refreshHistory(); } }
  }
  function hideProject(p, until) {
    const end = until === "1h" ? Date.now() + 3600000 : until === "today" ? new Date().setHours(23, 59, 59, 999) : null;
    projCall(p, "hide", end ? { until: end } : {}, `${p} 숨김${end ? ` · ${until === "1h" ? "1시간" : "오늘"}` : ""} · 에이전트는 계속 작업`);
  }
  const unhideProject = (p) => projCall(p, "unhide", {}, `${p} 다시 보임`);
  // Close all: agents that are working / asking / holding a draft need a tick each; unticked ones stay open.
  const WHY_KO = { working: "작업 중", asking: "질문 대기", draft: "초안" };
  function closeProjectDlg(p, archive = false, needs = null) {
    const as = projAgents(p);
    // Agents that need an explicit OK: from the server's 409 when given, else from the current state.
    const risky = needs ? needs.map((n) => ({ name: n.name, why: [WHY_KO[n.why] || n.why, n.draft ? `"${String(n.draft).split("\n")[0].slice(0, 60)}"` : ""].filter(Boolean).join(" · ") }))
      : as.filter((a) => ["working", "asking"].includes(kindOf(a)) || a.draft?.text).map((a) => ({ name: a.name, why: [kindOf(a) === "working" ? "작업 중" : "", kindOf(a) === "asking" ? "질문 대기" : "", a.draft?.text ? `초안: "${a.draft.text.split("\n")[0].slice(0, 60)}"` : ""].filter(Boolean).join(" · ") }));
    const coord = as.some((a) => /^(BRIEF|ADASH)$/.test(a.name));
    const html = `<h3>${archive ? "보관" : "모두 종료"} · ${esc(p)}</h3>
      <p>${archive ? "살아 있는 에이전트를 모두 종료하고 이 프로젝트를 계속 숨깁니다. 지난 세션의 '보관된 프로젝트'에서 복원하고, 그때 살아 있던 세션을 한 번에 다시 열 수 있습니다." : "살아 있는 에이전트를 모두 종료합니다. 세션 기록은 남아 하나씩 또는 한꺼번에 다시 열 수 있습니다."}</p>
      <p class="muted small">대상 에이전트 ${as.length}개${as.length ? `: ${as.map((a) => esc(a.name)).join(", ")}` : ""} · 파일·세션·기록은 지우지 않습니다.</p>
      ${risky.length ? `<div class="close-warn"><b>확인이 필요한 에이전트</b><p class="muted small">작업 중이거나 질문·초안이 있는 에이전트입니다. 체크를 풀면 종료하지 않고 다시 묻습니다.</p>${risky.map((a) => `<label class="pc-row"><input type="checkbox" name="confirm" value="${esc(a.name)}" checked> <b>${esc(a.name)}</b> <span class="muted small">${esc(a.why)}</span></label>`).join("")}</div>` : ""}
      ${coord ? `<p class="close-warn">이 프로젝트에는 조정자 세션도 있습니다. 종료하면 진행 중인 조정 작업도 멈춥니다.</p>` : ""}
      <label class="pc-row"><input type="checkbox" name="force"> 강제 종료 (터미널을 바로 닫음)</label>`;
    dialog(html, archive ? "보관" : "종료", (fd) => {
      const confirm = fd.getAll("confirm").map(String), mode = fd.get("force") ? "force" : "graceful";
      const sum = (r) => { const res = r?.results || []; const ok = res.filter((x) => x.ok).length, bad = res.filter((x) => !x.ok); return `${p} · 종료됨 ${ok} · 남김/실패 ${bad.length}${bad.length ? `\n${bad.map((x) => `${x.name}: ${CLOSE_WHY[x.reason] || x.reason || x.error || "?"}`).join("\n")}` : ""}`; };
      projCall(p, archive ? "archive" : "close", { mode, confirm }, archive ? (r) => `${sum(r)} · 보관함` : sum);
    });
  }
  function hiddenProjHtml(mobile = false) {
    if (!hiddenProjects.length) return "";
    const open = expandedOffline.has("__hidden__");
    const rows = hiddenProjects.map((h) => `<div class="hp-row"><b>${esc(h.name)}</b><span class="muted small">에이전트 ${h.agents} · ${esc(untilTxt(h))}</span><span class="spacer"></span>${h.archived ? `<button data-pact="restore" data-proj="${esc(h.name)}">복원</button>${h.archivedSessions.length ? `<button data-pact="reopen-all" data-proj="${esc(h.name)}">모두 다시 열기 (${h.archivedSessions.length})</button>` : ""}` : `<button data-pact="unhide" data-proj="${esc(h.name)}">보이기</button>`}</div>`).join("");
    return `<div class="hidden-proj${mobile ? " m" : ""}"><button class="hp-toggle" data-toggle-offline="__hidden__">${open ? "▾" : "▸"} 숨긴 프로젝트 ${hiddenProjects.length}${hiddenProjects.some((x) => x.agents) ? (() => { const n = hiddenProjects.reduce((k, x) => k + (x.agents || 0), 0); return L2(` · 에이전트 ${n}개는 백그라운드에서 계속 작업 (내 차례·알림에서 제외)`, ` · ${n} agent${n === 1 ? "" : "s"} keep working in the background (left out of My turn and notifications)`); })() : ""}</button>${open ? rows : ""}</div>`;
  }
  function renderHiddenProj() { const el = $("#hidden-proj"); if (!el) return; const h = hiddenProjHtml(); el.hidden = !h; if (h) H("#hidden-proj").innerHTML = h; }
  document.addEventListener("click", (e) => {
    const mb = e.target.closest("[data-proj-menu]");
    if (mb) { e.stopPropagation(); return openProjMenu(mb); }
    const b = e.target.closest("[data-pact]");
    if (!b) return;
    const p = b.dataset.proj, a = b.dataset.pact;
    $("#proj-menu")?.setAttribute("hidden", "");
    if (a === "hide") hideProject(p, b.dataset.until);
    else if (a === "unhide") unhideProject(p);
    else if (a === "close") closeProjectDlg(p);
    else if (a === "archive") closeProjectDlg(p, true);
    else if (a === "restore") projCall(p, "restore", {}, `${p} 복원 · 에이전트는 닫힌 채로 남음`);
    else if (a === "reopen-all") projCall(p, "reopen-all", {}, (r) => `${p} · 다시 연 세션 ${(r?.results || []).filter((x) => x.ok).length}개`);
  });

  // ---------- past sessions (지난 세션): browse/filter every recorded main session, view it read-only
  // or resume it in a new tmux window. Empty filter = most recent; the filter matches titles, adash
  // names, project and the first prompt (spaces ignored). Live sessions open their chat instead.
  const sess = { q: "", seq: 0, items: [] };
  const sessLabel = (h) => [h.agent, h.title || h.file.split("/").pop()].filter(Boolean).join(" · ");
  function openSessions() {
    openViewer(`<div class="sess-head"><b>지난 세션</b><input id="sess-q" type="search" placeholder="제목·이름·프로젝트·첫 지시로 찾기" autocomplete="off"><span class="muted small" id="sess-n"></span></div>${hiddenProjects.some((h) => h.archived) ? `<div class="sess-arch"><b>보관된 프로젝트</b>${hiddenProjects.filter((h) => h.archived).map((h) => `<div class="hp-row"><b>${esc(h.name)}</b><span class="muted small">${L2(`보관할 때 살아 있던 세션 ${h.archivedSessions.length}개`, `${h.archivedSessions.length} session(s) live at archive time`)}</span><span class="spacer"></span><button data-pact="restore" data-proj="${esc(h.name)}">복원</button>${h.archivedSessions.length ? `<button data-pact="reopen-all" data-proj="${esc(h.name)}">모두 다시 열기</button>` : ""}</div>`).join("")}</div>` : ""}<div id="sess-list" class="sess-list"><p class="muted">불러오는 중…</p></div>`, true);
    const i = $("#sess-q");
    i.value = sess.q;
    i.focus();
    let t = null;
    i.addEventListener("input", () => { clearTimeout(t); t = setTimeout(() => loadSessions(i.value), 200); });
    $("#sess-list").addEventListener("click", (e) => {
      const b = e.target.closest("[data-sess]");
      const h = b && sess.items[+b.dataset.sess];
      if (!h) return;
      if (b.dataset.do === "chat") { closeViewer(); openChat(h.agent); } else openHit({ ...h, fromList: true });
    });
    loadSessions(sess.q);
  }
  async function loadSessions(q) {
    sess.q = q;
    const seq = ++sess.seq;
    try {
      const r = await api("GET", `/api/sessions/search?q=${enc(q)}&limit=60`, undefined, { quiet: true });
      if (seq !== sess.seq || !$("#sess-list")) return;
      sess.items = (r?.sessions || []).filter((h) => inScope(h.project));
      $("#sess-n").textContent = `${sess.items.length}개`;
      $("#sess-list").innerHTML = sess.items.length
        ? sess.items.map((h, k) => {
          const label = sessLabel(h);
          const live = h.live && agentBy(h.agent);
          const acts = live
            ? `<button data-sess="${k}" data-do="chat">채팅 열기</button>`
            : `<button data-sess="${k}" data-do="view">보기</button>${FIXTURE ? "" : `<button class="primary" data-reopen-file="${esc(h.file)}" data-label="${esc(h.agent || h.title || "")}">이어서 열기</button>`}`;
          const prompt = h.prompt && h.prompt !== h.title && !DEMO.mask ? `<span class="muted small sess-p">${esc(h.prompt)}</span>` : "";
          return `<div class="sess-row"><div class="sess-main"><b>${esc(label)}</b><span class="muted small">${esc(h.project || "")} · ${esc(h.agentType)} · ${h.lastTs ? esc(ago(h.lastTs)) : ""}${live ? " · 실행 중" : ""}</span>${prompt}</div><div class="sess-act">${acts}</div></div>`;
        }).join("")
        : `<p class="muted">일치하는 세션 없음</p>`;
    } catch (e) { if (seq === sess.seq && $("#sess-list")) $("#sess-list").innerHTML = `<p>불러오기 실패: ${esc(e.message)}</p>`; }
  }
  $("#pal-input").addEventListener("input", palInput);
  $("#palette").addEventListener("click", (e) => {
    const it = e.target.closest("[data-pal]"); if (it) return palRun(+it.dataset.pal);
    const m = e.target.closest("[data-pal-mode]"); if (m) { pal.mode = m.dataset.palMode; $("#pal-input").focus(); return palInput(); }
    if (e.target === $("#palette")) closePalette();
  });
  $("#slash-pop").addEventListener("mousedown", (e) => { e.preventDefault(); const it = e.target.closest("[data-sl]"); if (it) pickSlash(+it.dataset.sl); });

  // ---------- Markdown (vendored markdown-it + texmath + KaTeX) ----------
  // html:false escapes raw HTML; math is tokenized by texmath before emphasis rules run.
  const mdit = window.markdownit({ html: false, linkify: true, breaks: true, typographer: false });
  // Typst math inside $…$ (agents writing Typst decks emit `p_(beta)(bold(s)) = e^(-beta H)\/Z`): converted to
  // LaTeX first, only when there is no LaTeX command at all and a Typst-only construct shows up. A formula KaTeX
  // still rejects is shown as its source in code type instead of a red error.
  const TY_GREEK = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega Gamma Delta Theta Lambda Xi Pi Sigma Upsilon Phi Psi Omega".split(" ");
  const TY_WORD = { dot: "\\cdot", times: "\\times", quad: "\\quad", "arrow.r": "\\rightarrow", "arrow.l": "\\leftarrow", "arrow.r.double": "\\Rightarrow", sum: "\\sum", prod: "\\prod", integral: "\\int", oo: "\\infty", infinity: "\\infty", approx: "\\approx", partial: "\\partial", nabla: "\\nabla", "lt.eq": "\\le", "gt.eq": "\\ge", "eq.not": "\\ne", ln: "\\ln", log: "\\log", exp: "\\exp" };
  const TY_FN = { bold: "\\boldsymbol", italic: "\\mathit", upright: "\\mathrm", vec: "\\vec", hat: "\\hat", tilde: "\\tilde", sqrt: "\\sqrt", overline: "\\overline", cal: "\\mathcal", frac: "\\frac" };
  const TY_DETECT = new RegExp(`[_^]\\(|\\\\/|"[^"]*"|\\b(?:${Object.keys(TY_FN).join("|")})\\(|\\b(?:${TY_GREEK.join("|")}|dot|times|quad|arrow\\.[a-z.]+)\\b`);
  const isTypstMath = (s) => !/\\[A-Za-z]/.test(s) && TY_DETECT.test(s);
  // index of the ")" matching the "(" at i, or -1
  const tyClose = (s, i) => { for (let d = 0, k = i; k < s.length; k++) { if (s[k] === "(") d++; else if (s[k] === ")" && --d === 0) return k; } return -1; };
  const tyArgs = (s) => { const out = []; let d = 0, st = 0; for (let k = 0; k < s.length; k++) { if (s[k] === "(") d++; else if (s[k] === ")") d--; else if (s[k] === "," && !d) { out.push(s.slice(st, k)); st = k + 1; } } out.push(s.slice(st)); return out.map((x) => x.trim()); };
  function typstToTex(s) {
    let out = "";
    for (let i = 0; i < s.length; ) {
      const c = s[i];
      if ((c === "_" || c === "^") && s[i + 1] === "(") {
        const j = tyClose(s, i + 1); if (j < 0) throw new Error("unbalanced");
        out += `${c}{${typstToTex(s.slice(i + 2, j))}}`; i = j + 1; continue;
      }
      if (c === "\\" && s[i + 1] === "/") { out += "/"; i += 2; continue; }
      if (c === '"') { const j = s.indexOf('"', i + 1); if (j < 0) throw new Error("unclosed text"); out += `\\text{${s.slice(i + 1, j)}}`; i = j + 1; continue; }
      const w = /^[A-Za-z]+(?:\.[a-z]+)*/.exec(s.slice(i));
      if (w && !/[A-Za-z0-9]/.test(s[i - 1] || "")) {
        let word = w[0];
        if (TY_FN[word] && s[i + word.length] === "(") {
          const j = tyClose(s, i + word.length); if (j < 0) throw new Error("unbalanced");
          const args = tyArgs(s.slice(i + word.length + 1, j)).map(typstToTex);
          if (word === "frac" && args.length !== 2) throw new Error("frac needs 2 args");
          out += `${TY_FN[word]}${args.map((a) => `{${a}}`).join("")}`; i = j + 1; continue;
        }
        // longest dotted name we know ("arrow.r.double" before "arrow.r"); otherwise only the leading word
        while (word.includes(".") && !TY_WORD[word]) word = word.slice(0, word.lastIndexOf("."));
        if (TY_WORD[word]) { out += `${TY_WORD[word]} `; i += word.length; continue; }
        if (TY_GREEK.includes(word)) { out += `\\${word} `; i += word.length; continue; }
        out += word; i += word.length; continue;
      }
      out += c; i++;
    }
    return out;
  }
  const mathEngine = {
    renderToString(tex, opts) {
      // .mx keeps the source as written ($…$, Typst form included) for copying. "$" is written as &#36;: texmath
      // puts this HTML into its template with String.replace, where "$$" would collapse to "$".
      const src = opts?.displayMode ? `$$${tex}$$` : `$${tex}$`, srcAttr = esc(src).replace(/\$/g, "&#36;");
      try { return `<span class="mx" data-md="${srcAttr}">${window.katex.renderToString(isTypstMath(tex) ? typstToTex(tex) : tex, { ...opts, throwOnError: true })}</span>`; }
      catch { return `<code class="math-raw mx" data-md="${srcAttr}">${srcAttr}</code>`; }
    },
  };
  mdit.use(window.texmath, { engine: mathEngine, delimiters: ["dollars", "brackets"], katexOptions: { throwOnError: true, trust: false, strict: "ignore" } });
  mdit.validateLink = (url) => !/^\s*(javascript|vbscript|data):/i.test(url);
  const isWeb = (u) => /^(https?:|mailto:)/i.test(u);
  const isLocal = (u) => !isWeb(u) && !u.startsWith("#") && (/^file:/i.test(u) || !/^[a-z][a-z0-9+.-]*:/i.test(u));
  const localPath = (u) => { try { return decodeURIComponent(u); } catch { return u; } };
  // `v` (message id) busts the browser cache: a figure regenerated at the same path shows fresh in newer messages.
  const fixtureSizes = new Map(); // fixture: upload path → bytes, so demo bubbles show sizes
  const fileUrl = (agent, p, v) => agentApiPath(`/api/agents/${enc(agent)}/file?path=${enc(p)}${v != null && v !== "" ? `&v=${enc(v)}` : ""}`);
  const defaultImage = mdit.renderer.rules.image;
  mdit.renderer.rules.image = (tokens, idx, opts, env, self) => {
    const t = tokens[idx], src = t.attrGet("src") || "";
    if (isLocal(src) && env.agent) {
      const p = localPath(src);
      t.attrSet("src", fileUrl(env.agent, p, env.v));
      t.attrSet("data-path", p);
    }
    t.attrSet("loading", "lazy");
    t.attrJoin("class", "md-img");
    return defaultImage(tokens, idx, opts, env, self);
  };
  const defaultLinkOpen = mdit.renderer.rules.link_open || ((tokens, idx, opts, env, self) => self.renderToken(tokens, idx, opts));
  mdit.renderer.rules.link_open = (tokens, idx, opts, env, self) => {
    const t = tokens[idx], href = t.attrGet("href") || "";
    if (isWeb(href)) { t.attrSet("target", "_blank"); t.attrSet("rel", "noopener noreferrer"); }
    else if (isLocal(href) && env.agent) {
      const p = localPath(href);
      t.attrSet("href", fileUrl(env.agent, p, env.v));
      t.attrSet("data-local", p);
      t.attrSet("data-agent", env.agent);
    }
    return defaultLinkOpen(tokens, idx, opts, env, self);
  };
  const defaultFence = mdit.renderer.rules.fence;
  mdit.renderer.rules.fence = (tokens, idx, opts, env, self) => `<div class="code"><button class="copy" type="button" data-act="copy-code">${t_("복사")}</button>${defaultFence(tokens, idx, opts, env, self)}</div>`;
  const mdCache = new Map();
  // Display math ($$…$$) is a block of its own: a blank line goes before the opening $$ and after the closing one
  // (outside code fences), so the next line is a normal paragraph. Otherwise texmath emits the block inside a <p>,
  // the browser splits that <p>, and the loose text after it lands in a narrow grid column of the reply bubble.
  // An equation number is only the explicit "$$ … $$ (3)" form.
  const DM_CLOSE = /\$\$(\s*\(\s*[^()\s][^()]*\))?\s*$/;
  function isolateDisplayMath(src) {
    const out = [];
    let fence = null, inMath = false, blankNext = false;
    for (const line of String(src ?? "").split("\n")) {
      const t = line.trim();
      if (!inMath && /^(```|~~~)/.test(t)) { const f = t.slice(0, 3); fence = fence ? (fence === f ? null : fence) : f; }
      if (fence || /^(```|~~~)/.test(t)) { if (blankNext && t) out.push(""); blankNext = false; out.push(line); continue; }
      if (inMath) { out.push(line); if (DM_CLOSE.test(t)) { inMath = false; blankNext = true; } continue; }
      if (t.startsWith("$$")) {
        if (out.length && out[out.length - 1].trim() !== "") out.push("");
        out.push(line);
        // one-line block "$$ x $$" / "$$ x $$ (3)", or an opening "$$" that closes on a later line
        if (t.length > 2 && DM_CLOSE.test(t.slice(2))) blankNext = true; else inMath = true;
        continue;
      }
      if (blankNext && t) out.push("");
      blankNext = false;
      out.push(line);
    }
    return out.join("\n");
  }
  const renderMd = (src, env) => mdit.render(isolateDisplayMath(src), env);
  function md(src, agent, v) {
    const key = `${agent || ""}\u0000${v ?? ""}\u0000${src}`;
    let h = mdCache.get(key);
    if (h === undefined) {
      h = renderMd(String(src ?? ""), { agent, v });
      if (mdCache.size > 3000) mdCache.clear();
      mdCache.set(key, h);
    }
    return h;
  }

  // ---------- rich blocks in rendered Markdown: numeric-table charts, ```svg, ```mermaid ----------
  const SERIES_VARS = ["--c1", "--c2", "--c3", "--c4", "--c5", "--c6"];
  const seriesColor = (k) => `var(${SERIES_VARS[k % SERIES_VARS.length]})`;
  let chartsOn = localStorage.getItem("adash.charts") !== "off";
  document.body.classList.toggle("charts-off", !chartsOn);

  // Cell text with KaTeX turned back into its TeX source ($…$), so `2.7\times10^{-4}` parses as a number.
  function cellSource(cell) {
    const c = cell.cloneNode(true);
    c.querySelectorAll(".katex").forEach((k) => {
      const tex = k.querySelector('annotation[encoding="application/x-tex"]')?.textContent;
      k.replaceWith(tex != null ? `$${tex}$` : k.textContent);
    });
    return c.textContent.replace(/\s+/g, " ").trim();
  }
  // Number + unit from a table cell: 1,234 · 12% · 15배 · 3 ms · 1e-3 · 2.7\times10^{-4} · 10^{-3} · \sim 5. Null when not numeric.
  function parseNum(raw) {
    let s = String(raw).replace(/\*\*|__|`|\$/g, "").replace(/\\(?:sim|approx)\b|[~≈∼]/g, "").replace(/\\[,;!]/g, " ");
    s = s.replace(/\\text\{([^}]*)\}|\\mathrm\{([^}]*)\}/g, "$1$2").replace(/\\times|×|\\cdot|·/g, "x").replace(/−/g, "-").replace(/\\%/g, "%").replace(/\s+/g, " ").trim();
    const unitOk = (u) => u.length <= 12 && !/\d/.test(u);
    let m = s.match(/^([+-]?\d[\d,]*(?:\.\d+)?)\s*x\s*10\^\{?\s*([+-]?\d+)\s*\}?\s*(.*)$/i);
    if (m && unitOk(m[3])) return { v: parseFloat(m[1].replace(/,/g, "")) * 10 ** +m[2], unit: m[3].trim() };
    m = s.match(/^([+-]?)10\^\{?\s*([+-]?\d+)\s*\}?\s*(.*)$/);
    if (m && unitOk(m[3])) return { v: (m[1] === "-" ? -1 : 1) * 10 ** +m[2], unit: m[3].trim() };
    m = s.match(/^([+-]?(?:\d[\d,]*(?:\.\d+)?|\.\d+)(?:e[+-]?\d+)?)\s*(.*)$/i);
    if (m && unitOk(m[2]) && !/^\d{4}-\d/.test(s)) return { v: parseFloat(m[1].replace(/,/g, "")), unit: m[2].trim() };
    return null;
  }

  function chartTable(table) {
    table.dataset.chart = "1";
    const ths = [...table.querySelectorAll("thead th")];
    const head = ths.map(cellSource);
    const rows = [...table.querySelectorAll("tbody tr")].map((tr) => [...tr.children]);
    if (rows.length < 2 || rows.length > 30 || head.length < 2) return;
    const parsed = rows.map((cells) => cells.map((c) => parseNum(cellSource(c))));
    const numCols = [];
    for (let j = 1; j < head.length; j++) if (parsed.filter((r) => r[j]).length >= Math.max(2, Math.ceil(rows.length / 2))) numCols.push(j);
    if (!numCols.length || numCols.length > 8) return;
    // Ratio/derived columns (e.g. "줄어든 비율", values in 배/x) compare across rows, so they get one shared scale.
    const isRatio = numCols.map((j) => /비율|ratio|배율|speed-?up/i.test(head[j]) || parsed.every((r) => !r[j] || /^(배|x)$/i.test(r[j].unit)));
    const seriesMode = rows.length >= 4 && parsed.every((r) => r[0]);
    const box = document.createElement("div");
    box.className = "tchart";
    const hidden = new Set();
    const legend = () => numCols.map((j, k) => `<button type="button" class="lg${hidden.has(k) ? " off" : ""}" data-k="${k}" title="${isRatio[k] ? esc(L2("비율 열: 행끼리 같은 축척", "ratio column: one scale across rows")) : ""}"><i style="background:${seriesColor(k)}"></i>${ths[j].innerHTML}${isRatio[k] ? `<span class="muted"> (${L2("비율", "ratio")})</span>` : ""}</button>`).join("");
    const scaleFor = (vals) => {
      const present = vals.filter((v) => v != null);
      const abs = present.filter((v) => v !== 0).map(Math.abs);
      if (!abs.length) return null;
      const max = Math.max(...abs), min = Math.min(...abs);
      // Log only for strictly positive panels spanning more than one decade; any value ≤ 0 keeps it linear.
      const log = present.every((v) => v > 0) && max / min > 10;
      const lo = log ? Math.log10(min) - 0.5 : 0; // half a decade below the smallest value so it still shows
      return { log, w: (v) => (v == null ? 0 : v === 0 ? 0 : log ? Math.max(2, (100 * (Math.log10(Math.abs(v)) - lo)) / (Math.log10(max) - lo)) : (100 * Math.abs(v)) / max) };
    };
    const draw = () => {
      const vis = numCols.map((_, k) => k).filter((k) => !hidden.has(k));
      let body, caption;
      if (seriesMode) {
        // Rows are an ordered x axis (first column numeric): one line per numeric column on a shared axis.
        const all = vis.flatMap((k) => parsed.map((r) => r[numCols[k]]?.v)).filter((v) => v != null);
        const pos = all.length && all.every((v) => v > 0);
        const log = pos && Math.max(...all) / Math.min(...all) > 10;
        const f = (v) => (log ? Math.log10(v) : v);
        const lo = all.length ? Math.min(0, ...all.map(f)) : 0, hi = all.length ? Math.max(...all.map(f)) : 1;
        const yMin = log ? Math.min(...all.map(f)) : lo, span = hi - yMin || 1;
        const W = 600, Hh = 170, px = 36, py = 12;
        const X = (i) => px + ((W - 2 * px) * i) / Math.max(1, rows.length - 1);
        const Y = (v) => Hh - py - ((Hh - 2 * py) * (f(v) - yMin)) / span;
        const lines = vis.map((k) => {
          const pts = parsed.map((r, i) => (r[numCols[k]] ? [X(i), Y(r[numCols[k]].v), r[numCols[k]].v, i] : null)).filter(Boolean);
          return `<polyline fill="none" stroke="${seriesColor(k)}" stroke-width="1.6" points="${pts.map((p) => `${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(" ")}"></polyline>` +
            pts.map((p) => `<circle cx="${p[0].toFixed(1)}" cy="${p[1].toFixed(1)}" r="2.5" fill="${seriesColor(k)}"><title>${esc(`${head[0]} ${cellSource(rows[p[3]][0])} · ${head[numCols[k]]}: ${cellSource(rows[p[3]][numCols[k]])}`)}</title></circle>`).join("");
        }).join("");
        const xl = rows.map((cells, i) => `<text x="${X(i).toFixed(1)}" y="${Hh - 1}" text-anchor="middle">${esc(cellSource(cells[0]))}</text>`).join("");
        body = `<svg class="tline" viewBox="0 0 ${W} ${Hh + 12}" preserveAspectRatio="xMidYMid meet"><line x1="${px}" x2="${W - px}" y1="${Hh - py}" y2="${Hh - py}" class="ax"></line>${lines}${xl}</svg>`;
        caption = L2(`${esc(head[0])} 순서${log ? " · 로그 축척 (값 범위 10배 초과)" : ""}`, `${esc(head[0])} order${log ? " · log scale (range over 10×)" : ""}`);
      } else {
        // One small panel per row; plain series scaled per panel, ratio series on one scale shared by all rows.
        const ratioScale = new Map(vis.filter((k) => isRatio[k]).map((k) => [k, scaleFor(parsed.map((r) => r[numCols[k]]?.v))]));
        let logCount = 0;
        body = `<div class="cpanels">${rows.map((cells, i) => {
          const sc = scaleFor(vis.filter((k) => !isRatio[k]).map((k) => parsed[i][numCols[k]]?.v));
          if (sc?.log) logCount++;
          const bars = vis.map((k) => {
            const j = numCols[k], p = parsed[i][j], s = isRatio[k] ? ratioScale.get(k) : sc;
            return `<div class="cb"><i class="cb-track">${p && s ? `<b style="width:${s.w(p.v).toFixed(1)}%;background:${seriesColor(k)}"></b>` : ""}</i><span class="cb-v">${p ? cells[j].innerHTML : "–"}</span></div>`;
          }).join("");
          return `<div class="cp"><div class="cp-t">${cells[0].innerHTML}${sc?.log ? `<span class="cp-log">${t_("로그")}</span>` : ""}</div>${bars}</div>`;
        }).join("")}</div>`;
        caption = L2(`행별 축척${logCount ? ` · 로그 축척 ${logCount}개 패널 (값 범위 10배 초과)` : ""}${isRatio.some(Boolean) ? " · 비율 열은 행끼리 같은 축척" : ""}`, `per-row scale${logCount ? ` · log scale in ${logCount} panel${logCount > 1 ? "s" : ""} (range over 10×)` : ""}${isRatio.some(Boolean) ? " · ratio columns share one scale across rows" : ""}`);
      }
      box.innerHTML = `<div class="tchart-bar"><span class="tc-legend">${legend()}</span><span class="spacer"></span><button type="button" class="chart-tg">${t_(chartsOn ? "차트 숨기기" : "차트 보기")}</button></div><div class="tchart-body">${body}<div class="tc-cap">${caption}</div></div>`;
    };
    draw();
    box.addEventListener("click", (e) => {
      const lg = e.target.closest(".lg");
      if (lg) { const k = +lg.dataset.k; hidden.has(k) ? hidden.delete(k) : hidden.add(k); if (hidden.size === numCols.length) hidden.delete(k); draw(); return; }
      if (e.target.closest(".chart-tg")) {
        chartsOn = !chartsOn;
        localStorage.setItem("adash.charts", chartsOn ? "on" : "off");
        document.body.classList.toggle("charts-off", !chartsOn);
        $$(".chart-tg").forEach((b) => (b.textContent = t_(chartsOn ? "차트 숨기기" : "차트 보기")));
      }
    });
    table.after(box);
  }

  // ```svg → <img src="data:…">: scripts and foreignObject stay inert; theme variables are injected as a <style>.
  function svgDataUrl(src) {
    let s = String(src).trim().replace(/^<\?xml[^>]*>\s*/i, "");
    if (!/^<svg[\s>]/i.test(s)) return null;
    const cs = getComputedStyle(document.documentElement), g = (n) => cs.getPropertyValue(n).trim();
    const vars = { "--fg": g("--fg"), "--muted": g("--muted"), "--border": g("--border"), "--surface": g("--card"), "--accent": g("--accent"), "--success": g("--warm"), "--warning": g("--info-warn"), "--error": g("--cold") };
    for (const v of SERIES_VARS) vars[v] = g(v);
    const style = `<style>svg{${Object.entries(vars).map(([k, v]) => `${k}:${v};`).join("")}color:${vars["--fg"]};font-family:${g("--font").replace(/"/g, "'")}}</style>`;
    if (!/^<svg[^>]*\sxmlns=/i.test(s)) s = s.replace(/^<svg/i, '<svg xmlns="http://www.w3.org/2000/svg"');
    s = s.replace(/^(<svg[^>]*>)/i, `$1${style}`);
    return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(s)}`;
  }
  const srcToggle = (src) => `<details class="src-tg"><summary>${t_("소스 보기")}</summary><pre>${esc(src)}</pre></details>`;
  function renderSvgBlock(el) {
    const url = svgDataUrl(el._src);
    el.innerHTML = url ? `<img class="svg-img" alt="svg" src="${url}">${srcToggle(el._src)}` : `<div class="rich-err">SVG 가 아닙니다</div><pre>${esc(el._src)}</pre>`;
  }

  // ```mermaid → vendored mermaid (lazy-loaded once), strict security, theme follows light/dark.
  let mermaidLoad = null, mermaidSeq = 0;
  const loadMermaid = () => (mermaidLoad ||= new Promise((res, rej) => {
    const s = document.createElement("script");
    s.src = "vendor/mermaid.min.js";
    s.onload = () => (window.mermaid ? res(window.mermaid) : rej(new Error("mermaid 없음")));
    s.onerror = () => { mermaidLoad = null; rej(new Error("mermaid 로드 실패")); };
    document.head.appendChild(s);
  }));
  async function renderMermaidBlock(el) {
    const src = el._src;
    try {
      const m = await loadMermaid();
      const dark = matchMedia("(prefers-color-scheme: dark)").matches;
      m.initialize({ startOnLoad: false, securityLevel: "strict", theme: dark ? "dark" : "neutral", fontFamily: getComputedStyle(document.body).fontFamily });
      const id = `mmd-${++mermaidSeq}`;
      let svg;
      try { ({ svg } = await m.render(id, src)); }
      finally { document.getElementById(`d${id}`)?.remove(); document.getElementById(id)?.remove(); } // mermaid's temporary render nodes
      el.innerHTML = `<div class="mmd-svg">${svg}</div>${srcToggle(src)}`;
    } catch (e) {
      el.innerHTML = `<div class="rich-err">${t_("mermaid 오류:")} ${esc(String(e?.message || e).split("\n")[0])}</div><pre>${esc(src)}</pre>`;
    }
  }

  function enhanceRich(root) {
    if (!root) return;
    root.querySelectorAll(".md table:not([data-chart])").forEach((t) => { try { chartTable(t); } catch (e) { console.warn("chart", e); } });
    for (const [lang, cls, fn] of [["svg", "svgblock", renderSvgBlock], ["mermaid", "mmdblock", renderMermaidBlock]]) {
      root.querySelectorAll(`.md pre > code.language-${lang}`).forEach((code) => {
        const el = document.createElement("div");
        el.className = `rich ${cls}`;
        el._src = code.textContent;
        (code.closest(".code") || code.parentElement).replaceWith(el);
        fn(el);
      });
    }
  }
  // Re-theme inline SVG/mermaid when the color scheme flips.
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    $$(".svgblock").forEach(renderSvgBlock);
    $$(".mmdblock").forEach(renderMermaidBlock);
  });

  // ---------- chat overlay ----------
  const loadedAt = Date.now();
  function openChat(name, tabName) {
    if (name !== ov.name) {
      // Composer state is per agent: the text draft and attachments (chips, uploads, failures) stay with their agent.
      if (ov.name) composerTextBy.set(ov.name, $("#composer-text").value);
      ov.name = name; ov.tx = null; ov.pending = []; ov.held = null; ov.openGroups.clear(); ov.lastLogHtml = ""; ov.lastInfoHtml = ""; ov.view = "chat";
      $("#send-result").textContent = ""; $("#composer-text").value = composerTextBy.get(name) || ""; renderAtts();
      $("#chat-log").innerHTML = ""; $("#chat-log")._h = null;
      setView("chat");
      $("#new-pill").hidden = true;
    }
    if (name) selName = name;
    ov.open = true;
    ov.tab = tabName || (name ? "info" : ov.tab === "info" ? "threads" : ov.tab);
    if (ov.tab === "meta") loadMeta();
    $("#overlay").hidden = false;
    render();
    if (name) pollTranscript(true);
  }
  function closeChat() {
    ov.open = false; $("#overlay").hidden = true;
    render();
  }

  async function pollTranscript(initial = false) {
    if (ov.polling || !ov.name) return;
    const name = ov.name;
    ov.polling = true;
    try {
      const after = ov.tx && ov.tx.source !== "terminal" && ov.tx.cursor ? `&after=${enc(ov.tx.cursor)}` : "";
      const r = await api("GET", `/api/agents/${enc(name)}/transcript?limit=200${after}`, undefined, { quiet: true });
      if (name !== ov.name) return;
      // replace:true (e.g. a btw inserted mid-conversation) = the full list again; otherwise append new ids and
      // refresh ones the server re-sent (btw status pending → answered, streaming answers).
      if (!ov.tx || r.replace || r.source === "terminal" || ov.tx.source !== r.source) ov.tx = { ...r, messages: r.messages || [] };
      else {
        const fresh = new Map((r.messages || []).map((m) => [m.id, m]));
        const kept = ov.tx.messages.map((m) => (fresh.has(m.id) ? fresh.get(m.id) : m));
        const seen = new Set(kept.map((m) => m.id));
        ov.tx = { ...r, messages: kept.concat((r.messages || []).filter((m) => !seen.has(m.id))) };
      }
      ov.tx.unavailable = false;
    } catch (e) {
      if (name !== ov.name) return;
      // Backend without the transcript endpoint: fall back to the last message from /api/state.
      const a = agentBy(name);
      ov.tx = { source: null, mappingConfidence: null, unavailable: e.message, cursor: null, messages: a?.lastMessage ? [{ id: "last", role: "assistant", text: a.lastMessage, ts: a.stateSince, tool: null }] : [] };
    } finally { ov.polling = false; }
    if (ov.open && name === ov.name) { renderInfo(); renderWorkLine(); }
    renderChat(initial);
  }

  function renderOverlay() {
    const a = ov.name && agentBy(ov.name);
    const order = triageOrder();
    const k = order.indexOf(ov.name);
    $("#ov-title").innerHTML = a ? `<span class="${a.ambiguous ? "amb" : ""}" title="${esc(`${L2("이름 출처", "name source")}: ${a.nameSource || "?"}${a.ambiguous ? ` · ${L2("이름이 모호함", "ambiguous name")}` : ""}`)}">${esc(a.name)}${a.ambiguous ? "?" : ""}</span>${kindOf(a) === "working" ? WKa(a.name) : isMyTurn(model.agents.find((x) => x.name === a.name)) ? TK(model.agents.find((x) => x.name === a.name)) : ""} <span class="muted small">${esc(a.project)}${a.role ? " · " + esc(a.role) : ""}</span>` : ov.name ? esc(ov.name) : "패널";
    $("#ov-nav").textContent = `내 차례 ${k >= 0 ? k + 1 : "–"}/${model.counts.mine}`;
    $("#ov-unsnooze").hidden = !a?.snoozedUntil;
    // Live agent → 종료/강제 종료; ended (offline) agent → 다시 열기.
    $("#ov-close-agent").hidden = $("#ov-force-agent").hidden = $("#ov-restart-agent").hidden = !a;
    $("#ov-reopen-agent").hidden = !!a;
    $$(".tabs [data-tab]").forEach((b) => b.classList.toggle("on", b.dataset.tab === ov.tab));
    $$(".side section").forEach((s) => s.classList.toggle("on", s.dataset.pane === ov.tab));
    const nThreads = (state.threads || []).filter((t) => ["held_possible_draft", "escalated", "delivery_unconfirmed", "failed"].includes(t.status)).length;
    const nTasks = (state.humanTasks || []).filter((t) => !t.done).length;
    $(".tabs [data-tab=threads]").textContent = `검토 요청${nThreads ? ` (${nThreads})` : ""}`;
    $(".tabs [data-tab=tasks]").textContent = `내 할 일${nTasks ? ` (${nTasks})` : ""}`;
    $("#composer").hidden = !ov.name;
    renderInfo(); renderThreads(); renderTasks(); renderWorkLine(); renderHeld(); renderAskCard(); renderAuthCard(); renderRelayCard(); renderMagiStrip(); renderDecisions(); renderDraftStrip(); renderQueuedStrip(); updatePrefixWarn(); renderBtwNote();
    if (!ov.name) { H("#chat-log").innerHTML = `<p class="muted">보드에서 에이전트를 고르면 대화가 여기에 열립니다.</p>`; }
  }
  // ---------- triage: ack, next unread (A2) ----------
  function nextTriage(fromName) {
    const order = triageOrder();
    const i = order.indexOf(fromName);
    const rest = order.filter((n) => n !== fromName);
    if (!rest.length) { toast("남은 미확인 없음", 2000); return null; }
    const next = i >= 0 && i < order.length - 1 ? order[i + 1] : rest[0];
    if (ov.open) openChat(next, "info");
    else { selName = next; render(); requestAnimationFrame(() => $(".lrow.sel")?.scrollIntoView({ block: "nearest" })); }
    return next;
  }
  async function ackAndNext(name) {
    if (!name) return;
    try { await api("POST", `/api/agents/${enc(name)}/ack`); } catch { return; }
    // Optimistic: drop it from the unread set now; the next /api/state confirms.
    // Optimistic, mirroring the server: acknowledging also answers the open decisions (decisions → []).
    const s = agentBy(name); if (s) { s.unread = false; s.ackedAt = Date.now(); if (decisionsOf(s).length) { s.decisions = []; s.decisionsAnswered = true; } }
    render();
    // Nothing left on my turn: from the chat, close it and go home (phones: the 내 차례 list).
    if (ov.open && !triageOrder().some((n) => n !== name)) {
      closeChat();
      if (isMobile()) { mtab = "mine"; localStorage.setItem("adash.mtab", mtab); render(); window.scrollTo(0, 0); }
      toast("모두 확인했습니다", 2500);
      return;
    }
    nextTriage(name);
  }

  // ---------- "정하실 것" checklist pinned at the top of the chat (A1) ----------
  const itemRe = (n) => new RegExp(`^\\s*${n}\\.(\\s|$)`);
  function composerItem(n) { const l = $("#composer-text").value.split("\n").find((x) => itemRe(n).test(x)); return l == null ? null : l.replace(/^\s*\d+\.\s?/, "").trim(); }
  function setComposerItem(n, text) {
    const ta = $("#composer-text");
    const lines = ta.value ? ta.value.split("\n") : [];
    const line = `${n}. ${text}`;
    let i = lines.findIndex((x) => itemRe(n).test(x));
    if (i >= 0) lines[i] = line;
    else {
      i = lines.findIndex((x) => { const m = x.match(/^\s*(\d+)\./); return m && +m[1] > n; });
      if (i < 0) i = lines.length;
      lines.splice(i, 0, line);
    }
    ta.value = lines.join("\n");
    const end = lines.slice(0, i + 1).join("\n").length;
    ta.focus(); ta.setSelectionRange(end, end);
    renderDecisions();
  }
  // Fold state per agent + decision bundle (v46): phones start folded, desktop open; a new bundle falls back to the default.
  const decKey = (name, ds) => { let h = 0; for (const c of ds.map((d) => `${d.text}|${(d.options || []).join("/")}`).join("\n")) h = (h * 31 + c.charCodeAt(0)) | 0; return `${name}#${(h >>> 0).toString(36)}`; };
  const decFoldMap = () => { try { return JSON.parse(localStorage.getItem("adash.decFold") || "{}"); } catch { return {}; } };
  function setDecFold(key, v) { const m = decFoldMap(); delete m[key]; m[key] = v; const ks = Object.keys(m); ks.slice(0, Math.max(0, ks.length - 60)).forEach((k) => delete m[k]); localStorage.setItem("adash.decFold", JSON.stringify(m)); }
  function renderDecisions() {
    const card = $("#decision-card");
    const a = ov.open && ov.name && agentBy(ov.name);
    const ds = decisionsOf(a);
    if (!ds.length) { card.hidden = true; H("#decision-card").innerHTML = ""; return; }
    card.hidden = false;
    const key = decKey(ov.name, ds), picked = ds.filter((_, i) => composerItem(i + 1)).length;
    // Choosing (an answer already in the composer) keeps it open unless the user folded it explicitly.
    const stored = decFoldMap()[key], folded = stored ?? (isMobile() && !picked);
    card.dataset.key = key; card.classList.toggle("folded", folded);
    const first = String(ds[0].text || "").replace(/[*_`#>[\]]/g, "").replace(/\s+/g, " ").trim();
    const pk = picked ? `<span class="dc-picked">${L2(`선택 ${picked}/${ds.length}`, `${picked}/${ds.length} chosen`)}</span>` : "";
    const tog = `<button class="dc-fold" data-act="dec-fold" aria-expanded="${!folded}" title="${esc(folded ? L2("펼치기", "Expand") : L2("접기", "Collapse"))}">${folded ? "▸" : "▾"}</button>`;
    if (folded) { H("#decision-card").innerHTML = `<div class="dc-head" data-act="dec-fold">${tog}<span class="dc-tag">${L2("정하실 것", "Decisions")} ${ds.length}</span><span class="dc-sum">${esc(first)}</span>${ds.length > 1 ? `<span class="dc-more">+${ds.length - 1}</span>` : ""}${pk}</div>`; return; }
    H("#decision-card").innerHTML = `<div class="dc-head">${tog}<span class="dc-tag">${L2("정하실 것", "Decisions")} ${ds.length}</span>${pk}<span class="muted small dc-hint">${L2("항목이나 선택지를 누르면 입력창에 번호와 함께 채워집니다", "Tap an item or option to fill the composer with its number")}</span><span class="spacer"></span><button data-act="dec-all">${L2("모두 답하기", "Answer all")}</button></div>
      <ol class="dc-list">${ds.map((d, i) => { const n = i + 1, c = composerItem(n); return `<li class="${c ? "done" : ""}"><button class="dc-item" data-act="dec-item" data-n="${n}">${mdit.renderInline(String(d.text || ""))}</button>${(d.options || []).map((o) => `<button class="dc-opt${c === o ? " on" : ""}" data-act="dec-opt" data-n="${n}" data-o="${esc(o)}">${esc(o)}</button>`).join("")}</li>`; }).join("")}</ol>`;
  }
  function toggleDecFold() { const c = $("#decision-card"); if (!c.dataset.key) return; setDecFold(c.dataset.key, !c.classList.contains("folded")); renderDecisions(); }
  function answerAllDecisions() {
    const ds = decisionsOf(agentBy(ov.name));
    ds.forEach((_, i) => { if (composerItem(i + 1) == null) setComposerItem(i + 1, ""); });
    const ta = $("#composer-text"), lines = ta.value.split("\n");
    const first = lines.findIndex((x) => /^\s*\d+\.\s*$/.test(x));
    const at = first >= 0 ? lines.slice(0, first + 1).join("\n").length : ta.value.length;
    ta.focus(); ta.setSelectionRange(at, at);
  }

  // ---------- unsent text in the agent's terminal composer (A3) ----------
  ov.draftNoClear = new Set();
  // The harness took the message but holds it until its turn ends (agy, Claude Code queues).
  function renderQueuedStrip() {
    const el = $("#queued-strip"), a = ov.open && ov.name ? agentBy(ov.name) : null;
    if (!a?.queued) { el.hidden = true; el.innerHTML = ""; el._h = null; return; }
    const html = `<span class="muted small">${esc(L2("보낸 메시지가 대기열에 있습니다 · 에이전트 작업이 끝나면 전달됩니다", "Your message is queued · delivered when the agent's turn ends"))}</span><span class="spacer"></span><button data-send-now="${esc(a.name)}">${t_("지금 보내기")}</button>`;
    if (el._h !== html) { el.innerHTML = html; el._h = html; }
    el.hidden = false;
  }
  $("#queued-strip").addEventListener("click", async (e) => {
    const b = e.target.closest("[data-send-now]"); if (!b) return;
    b.disabled = true;
    try { await api("POST", `/api/agents/${enc(b.dataset.sendNow)}/send-now`, {}, { quiet: true }); toast(t_("지금 보냈습니다")); }
    catch (err) { toast(`${t_("보내지 못함:")} ${err.message}`); b.disabled = false; }
  });
  function renderDraftStrip() {
    const el = $("#draft-strip");
    const a = ov.open && ov.name && agentBy(ov.name);
    const d = a?.draft;
    if (!d?.text) { el.hidden = true; H("#draft-strip").innerHTML = ""; return; }
    el.hidden = false;
    const canClear = !ov.draftNoClear.has(a.name);
    H("#draft-strip").innerHTML = `<span class="ds-label">터미널 입력창에 보내지 않은 초안${d.readable === false ? " (일부만 읽힘)" : ""}:</span><span class="ds-text" title="${esc(d.text)}">"${esc(d.text.split("\n")[0].slice(0, 160))}"</span><span class="spacer"></span>
      <button data-act="draft-submit" title="입력창의 초안을 그대로 제출 (Enter)">이어서 보내기</button>${canClear ? `<button data-act="draft-clear">지우기</button>` : ""}<button data-act="draft-import">내 입력창으로 가져오기</button>`;
  }
  async function draftAction(action) {
    const name = ov.name;
    try {
      await api("POST", `/api/agents/${enc(name)}/draft`, { action });
      toast(action === "submit" ? "초안을 보냈습니다" : "초안을 지웠습니다", 2500);
      if (!FIXTURE) refresh();
    } catch (e) {
      // No reliable clear keystroke for this agent: hide the button from now on.
      if (action === "clear" && [404, 405, 409, 501].includes(e.status)) { ov.draftNoClear.add(name); renderDraftStrip(); }
    }
  }

  // ---------- cache countdown (A4) ----------
  function tickCountdowns() {
    const t = nowTick();
    for (const el of $$(".cd[data-exp]")) {
      const d = +el.dataset.exp - t;
      const s = Math.max(0, Math.floor(d / 1000));
      const txt = d <= 0 ? L2("식음", "expired") : `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
      if (el.dataset.p) el.innerHTML = `<svg viewBox="0 0 10 10" aria-hidden="true"><circle cx="5" cy="5" r="4" fill="none" stroke="currentColor" stroke-width="1.2"/><path d="M5 2.6V5l1.6 1" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>${txt}`;
      else el.textContent = txt;
      el.classList.toggle("soon", d > 0 && d <= soonSec * 1000);
      el.title = (el.dataset.label ? `${el.dataset.label} · ` : "") + (d <= 0 ? "캐시가 식었습니다" : `캐시가 식기까지 ${fmtMin(d / 60000) || "1분 미만"}`) + (el.dataset.basis ? `\n${el.dataset.basis}` : "");
    }
  }

  // ---------- terminal view: the agent's screen (colours kept) and keys sent to its pane ----------
  ov.view = "chat";
  const ANSI16 = ["#000", "#c33", "#3a3", "#c90", "#36c", "#c3c", "#3aa", "#ccc", "#666", "#f55", "#5d5", "#fd5", "#59f", "#f5f", "#5ff", "#fff"];
  const ansi256 = (n) => n < 16 ? ANSI16[n] : n >= 232 ? `rgb(${8 + (n - 232) * 10},${8 + (n - 232) * 10},${8 + (n - 232) * 10})` : (() => { n -= 16; const c = (x) => (x ? 55 + x * 40 : 0); return `rgb(${c(Math.floor(n / 36))},${c(Math.floor(n / 6) % 6)},${c(n % 6)})`; })();
  // SGR escapes → spans (fg/bg 16/256/truecolour, bold, dim, italic, underline, inverse); other escapes dropped
  function ansiHtml(text) {
    let st = {}, out = "";
    const style = () => { let fg = st.fg, bg = st.bg; if (st.inv) [fg, bg] = [bg || "var(--bg)", fg || "var(--fg)"]; const p = []; if (fg) p.push(`color:${fg}`); if (bg) p.push(`background:${bg}`); if (st.b) p.push("font-weight:700"); if (st.d) p.push("opacity:.6"); if (st.i) p.push("font-style:italic"); if (st.u) p.push("text-decoration:underline"); return p.join(";"); };
    for (const part of text.split(/(\x1b\[[0-9;:]*[A-Za-z])/)) {
      const m = part.match(/^\x1b\[([0-9;:]*)m$/);
      if (m) {
        const c = m[1] ? m[1].split(/[;:]/).map(Number) : [0];
        for (let i = 0; i < c.length; i++) {
          const x = c[i];
          if (x === 0) st = {}; else if (x === 1) st.b = 1; else if (x === 2) st.d = 1; else if (x === 3) st.i = 1; else if (x === 4) st.u = 1; else if (x === 7) st.inv = 1;
          else if (x === 22) st.b = st.d = 0; else if (x === 23) st.i = 0; else if (x === 24) st.u = 0; else if (x === 27) st.inv = 0;
          else if (x >= 30 && x <= 37) st.fg = ANSI16[x - 30]; else if (x >= 90 && x <= 97) st.fg = ANSI16[x - 82]; else if (x === 39) st.fg = null;
          else if (x >= 40 && x <= 47) st.bg = ANSI16[x - 40]; else if (x >= 100 && x <= 107) st.bg = ANSI16[x - 92]; else if (x === 49) st.bg = null;
          else if (x === 38 || x === 48) { const k = x === 38 ? "fg" : "bg"; if (c[i + 1] === 5) { st[k] = ansi256(c[i + 2]); i += 2; } else if (c[i + 1] === 2) { st[k] = `rgb(${c[i + 2]},${c[i + 3]},${c[i + 4]})`; i += 4; } }
        }
      } else if (part && !part.startsWith("\x1b")) { const s = style(); out += s ? `<span style="${s}">${esc(part)}</span>` : esc(part); }
    }
    return out;
  }
  async function pollTerminal() {
    const name = ov.name;
    try {
      const r = await api("GET", `/api/agents/${enc(name)}/screen`, undefined, { quiet: true });
      if (name !== ov.name) return;
      const html = ansiHtml((r?.text || "").replace(/\n+$/, "")) || esc(t_("(터미널 화면 없음)"));
      const tv = $("#term-view");
      if (tv._h !== html) { const bottom = tv.scrollHeight - tv.scrollTop - tv.clientHeight < 40; tv.innerHTML = html; tv._h = html; if (bottom) tv.scrollTop = tv.scrollHeight; }
    } catch (e) { H("#term-view").innerHTML = esc(`${t_("터미널 화면을 불러올 수 없음:")} ${e.message}`); }
  }
  function setView(v) {
    ov.view = v;
    $("#chat-log").hidden = v === "terminal"; $("#term-view").hidden = v !== "terminal"; $("#term-keys").hidden = v !== "terminal";
    $$("[data-view]").forEach((b) => b.classList.toggle("on", b.dataset.view === v));
    clearInterval(setView.t);
    if (v === "terminal") { $("#term-view")._h = null; pollTerminal(); setView.t = setInterval(() => { if (ov.open && ov.view === "terminal") pollTerminal(); }, 1000); }
  }
  async function termInput(body) {
    const name = ov.name; if (!name) return;
    try { await api("POST", `/api/agents/${enc(name)}/term-input`, body, { quiet: true }); setTimeout(pollTerminal, 150); }
    catch (e) { toast(`${t_("터미널에 보내지 못함:")} ${e.message}`); }
  }
  $("#term-keys").addEventListener("click", (e) => { const b = e.target.closest("[data-tkey]"); if (!b) return; const k = b.dataset.tkey; termInput(k.length === 1 ? { text: k } : { key: k }); });
  $("#term-keys").addEventListener("submit", (e) => { e.preventDefault(); const i = $("#term-text"); const text = i.value; i.value = ""; termInput({ text, key: "Enter" }); });

  // ---------- login prompt on the agent's screen (state "auth") ----------
  function renderAuthCard() {
    const card = $("#auth-card"), a = ov.open && ov.name ? agentBy(ov.name) : null;
    if (a?.state === "approval" && a.auth) {
      const t = a.auth;
      const html = `<div class="ask-head"><span class="relay-tag">${t_("승인 필요")}</span><span class="muted small">${esc(t.question || "")}</span><span class="spacer"></span><button data-act="view" data-v="terminal">${t_("터미널 보기")}</button></div>
        <div class="auth-row"><code class="path">${esc(t.detail || "")}</code></div>
        <div class="auth-row"><span class="spacer"></span>${(t.options || []).map((o) => `<button data-approve="${o.n}"${o.selected ? ' class="primary"' : ""}>${esc(o.label)}</button>`).join("")}</div>`;
      if (card._h !== html) { card.innerHTML = html; card._h = html; }
      card.hidden = false; return;
    }
    if (a?.state === "trust" && a.auth) {
      const t = a.auth, ok = Number.isInteger(t.yes);
      const html = `<div class="ask-head"><span class="relay-tag">${t_("폴더 신뢰 필요")}</span><span class="muted small">${esc(t.question || "")}</span><span class="spacer"></span><button data-act="view" data-v="terminal">${t_("터미널 보기")}</button></div>
        <div class="auth-row"><code class="path auth-url" title="${esc(t.cwd || "")}">${esc(t.cwd || "")}</code></div>
        ${t.queued ? `<div class="auth-row small">${esc(L2(`보류된 메시지 ${t.queued}개 · 신뢰하면 자동으로 보냅니다`, `${t.queued} message(s) waiting · sent once trusted`))}</div>` : ""}
        <div class="auth-row muted small">${t_("신뢰하면 이 폴더의 설정과 훅이 실행될 수 있습니다.")}</div>
        <div class="auth-row"><span class="spacer"></span>${Number.isInteger(t.no) ? `<button data-trust="0">${t_("거절")}</button>` : ""}<button class="primary" data-trust="1"${ok ? "" : " disabled"}>${t_("이 폴더 신뢰")}</button></div>`;
      if (card._h !== html) { card.innerHTML = html; card._h = html; }
      card.hidden = false; return;
    }
    const au = a?.state === "auth" ? a.auth : null;
    if (!au) { card.hidden = true; card.innerHTML = ""; card._h = null; return; }
    // another process of the same harness works since this prompt appeared: the sign-in exists,
    // this process just has not re-read it
    const other = (state.agents || []).find((o) => o.agentType === a.agentType && o.name !== a.name && o.handle && o.state !== "auth" && (o.stateSince || 0) > (au.since || 0));
    const html = `<div class="ask-head"><span class="relay-tag err-n">${t_("로그인이 필요합니다")}</span><span class="muted small">${esc(au.reason || "")}</span><span class="spacer"></span><button data-act="view" data-v="terminal">${t_("터미널에서 로그인")}</button><button class="primary" data-act="restart-agent" data-name="${esc(a.name)}" title="${esc(t_("이미 다른 창에서 로그인했다면: 정상 종료 후 같은 세션으로 다시 엶"))}">${t_("다시 시작")}</button></div>
      ${other ? `<div class="auth-row small">${esc(L2(`다른 창(${other.name})에서는 로그인돼 있어요 → 다시 시작하면 됩니다.`, `Signed in elsewhere (${other.name}) → restart this agent.`))}</div>` : `<div class="auth-row muted small">${t_("다른 창에서 이미 로그인했다면 '다시 시작', 아니면 터미널에서 로그인하세요.")}</div>`}
      ${au.url ? `<div class="auth-row"><a class="btn primary" href="${esc(au.url)}" target="_blank" rel="noopener noreferrer">${t_("브라우저에서 열기")}</a><code class="path auth-url" title="${esc(au.url)}">${esc(au.url)}</code></div>` : ""}
      ${au.code ? `<div class="auth-row"><span class="muted small">${t_("장치 코드")}</span><code class="auth-code">${esc(au.code)}</code><button data-copy="${esc(au.code)}">${t_("복사")}</button></div>` : ""}
      <form class="auth-row auth-paste"><input name="code" autocomplete="off" spellcheck="false" placeholder="${esc(t_("받은 코드를 붙여 넣고 Enter (터미널로 보냄)"))}"><button>${t_("보내기")}</button></form>`;
    if (card._h !== html) { card.innerHTML = html; card._h = html; }
    card.hidden = false;
  }
  $("#auth-card").addEventListener("click", async (e) => {
    const ap = e.target.closest("[data-approve]");
    if (ap && ov.name) {
      const name = ov.name; ap.disabled = true;
      try { const r = await api("POST", `/api/agents/${enc(name)}/approve`, { option: +ap.dataset.approve }, { quiet: true }); toast(`${name}: ${r?.chose || ""}`, 4000); }
      catch (err) { toast(`${t_("처리하지 못함:")} ${err.message}`, 6000); ap.disabled = false; }
      return;
    }
    const b = e.target.closest("[data-trust]"); if (!b || !ov.name) return;
    const name = ov.name, accept = b.dataset.trust === "1";
    b.disabled = true;
    try {
      const r = await api("POST", `/api/agents/${enc(name)}/trust`, { accept }, { quiet: true });
      toast(accept ? `${name}: ${t_("폴더를 신뢰했습니다")}${r?.queuedDelivered ? ` · ${L2(`보류 메시지 ${r.queuedDelivered}개 보냄`, `${r.queuedDelivered} queued message(s) sent`)}` : ""}` : `${name}: ${t_("거절했습니다")}`, 5000);
    } catch (err) { toast(`${t_("처리하지 못함:")} ${err.message}`, 6000); b.disabled = false; }
  });
  $("#auth-card").addEventListener("submit", (e) => { e.preventDefault(); const i = e.target.querySelector("input"); const text = i.value.trim(); if (!text) return; i.value = ""; termInput({ text, key: "Enter" }); });

  // ---------- review reply parked for this requester (relay_held) ----------
  function renderRelayCard() {
    const card = $("#relay-card");
    const ts = ov.open && ov.name ? heldRelays(ov.name) : [];
    if (!ts.length) { card.hidden = true; H("#relay-card").innerHTML = ""; return; }
    card.hidden = false;
    H("#relay-card").innerHTML = ts.map((t) => `<div class="relay-item">
      <div class="ask-head"><span class="relay-tag">답변 보류</span><span class="muted small">${t.heldSince ? `${ago(t.heldSince)}부터` : ""}</span><span class="spacer"></span>
        <button data-act="open-reply" data-dir="${esc(t.dir || "")}">답변 열기</button><button class="primary" data-act="relay-force" data-id="${esc(t.id)}" data-to="${esc(t.to)}">초안과 함께 전달</button></div>
      <div class="relay-msg">${esc(t.to)}의 검토 답변이 도착했지만 ${esc(HELD_KO[t.heldReason] || t.heldReason || "입력창 상태")} 때문에 전달되지 않았습니다.</div>
      ${t.replyPreview ? `<div class="relay-preview md">${md(t.replyPreview, ov.name, t.id)}</div>` : ""}</div>`).join("");
    enhanceRich(card);
  }

  // ---------- open ask / permission prompt (pinned above the composer) ----------
  ov.answerUnsupported = new Set();
  // Answers being composed for the open ask, keyed by question id: {optionIndex} | {optionIndices} | {text}.
  ov.askSel = { key: "", answers: new Map() };
  // `ask.questions` (A5); older backends only send one question/options/multi at the top level.
  const askQuestions = (q) => (Array.isArray(q.questions) && q.questions.length ? q.questions : [{ id: "q0", question: q.question, options: q.options, multi: q.multi, recommended: q.recommended }])
    .map((x, i) => ({ ...x, id: x.id ?? `q${i}`, options: x.options || [] }));
  const isAnswered = (v) => v && (v.optionIndex != null || (v.optionIndices && v.optionIndices.length) || (v.text && v.text.trim()));
  // While the harness shows a question (omp ask, Claude AskUserQuestion) the terminal input belongs to that picker:
  // a normal message would not arrive or would pick something. So the composer is hidden and the ask card (with its
  // 직접 답하기 field) takes its place; the unsent draft stays in the hidden textarea and comes back afterwards.
  function renderAskCard() {
    // a finished or failed answer job belongs to its question: gone once that question is
    // answered elsewhere (terminal) or another question replaced it
    for (const [n, j] of answerJobs) {
      if (j.state === "running") continue;
      const q = agentBy(n)?.ask;
      if (!q || (j.askedAt != null && q.askedAt !== j.askedAt)) answerJobs.delete(n);
    }
    renderAskCard0(); fitOvHead();
    const card = $("#ask-card"), asking = !card.hidden && !!ov.name;
    $("#composer").classList.toggle("ask-hidden", asking);
    $("#att-bar").classList.toggle("ask-hidden", asking);
    card.classList.toggle("ask-dock", asking);
    card.querySelector(".ask-foot")?.remove();
    if (!asking || answerJobs.get(ov.name)) return;
    const draft = $("#composer-text").value.trim();
    card.insertAdjacentHTML("beforeend", `<div class="ask-foot muted small">${draft ? `<span>${L2("작성 중이던 메시지는 질문이 끝나면 돌아옵니다", "Your unsent message comes back when the question ends")}</span>` : ""}<span class="spacer"></span><button type="button" class="linkish" data-act="ask-skip">${L2("질문 건너뛰고 메시지 보내기", "Skip the question and send a message")}</button></div>`);
  }
  function askSkipDlg(name) {
    dialog(`<h3>${esc(L2("질문 건너뛰기", "Skip the question"))}</h3><p>${esc(L2(`${name}의 질문 화면을 취소(중단 키)하고 메시지 입력창으로 돌아갑니다. 질문에는 답하지 않은 것으로 처리됩니다.`, `Cancels ${name}'s question (interrupt key) and returns to the message box. The question stays unanswered.`))}</p>`, L2("건너뛰기", "Skip"), async () => {
      try { await api("POST", `/api/agents/${enc(name)}/interrupt`); } catch (e) { toast(L2(`취소 실패: ${e.message}`, `Cancel failed: ${e.message}`)); return; }
      const s = agentBy(name); if (s) { s.ask = null; if (s.state === "asking") s.state = "waiting"; }
      renderAskCard(); $("#composer-text").focus();
      if (!FIXTURE) refresh();
    });
  }
  function renderAskCard0() {
    const card = $("#ask-card");
    const a = ov.open && ov.name && agentBy(ov.name);
    const job = ov.open && ov.name && answerJobs.get(ov.name);
    document.body.classList.toggle("answering", !!(job && job.state === "running"));
    if (job) { card.hidden = false; H("#ask-card").innerHTML = answerJobHtml(job); return; }
    const q = a && kindOf(a) === "asking" ? a.ask : null;
    if (!q) { card.hidden = true; H("#ask-card").innerHTML = ""; return; }
    card.hidden = false;
    const key = `${a.name}\u0000${q.askedAt || ""}`;
    if (ov.askSel.key !== key) ov.askSel = { key, answers: new Map() };
    const sel = ov.askSel.answers;
    const n = esc(a.name);
    const unsupported = ov.answerUnsupported.has(a.name);
    const qs = askQuestions(q);
    const recIdx = (x) => (typeof x.recommended === "number" ? x.recommended : x.options.findIndex((o) => o.recommended || /\(추천\)|\(recommended\)/i.test(o.label || "")));
    const sections = qs.map((x, qi) => {
      const v = sel.get(x.id) || {};
      const rec = recIdx(x);
      // Options are role=button divs (not <button>) so their Markdown descriptions can hold real links;
      // a click on a link inside is handled by the link (global handler returns before data-act).
      const opt = (o, i) => {
        const on = x.multi ? (v.optionIndices || []).includes(i) : v.optionIndex === i;
        return `<div role="button" tabindex="${unsupported ? -1 : 0}" class="ask-opt${on ? " on" : ""}${i === rec ? " rec" : ""}${unsupported ? " disabled" : ""}" ${unsupported ? 'aria-disabled="true"' : `data-act="ask-pick"`} data-q="${esc(x.id)}" data-i="${i}">
          <b>${x.multi ? `<span class="ck">${on ? "☑" : "☐"}</span>` : ""}${esc(o.label)}${i === rec ? `<span class="rec-tag">${t_("추천")}</span>` : ""}</b>${o.description ? `<div class="md ask-md muted">${md(o.description, a.name, `${q.askedAt || ""}:${x.id}:${i}`)}</div>` : ""}</div>`;
      };
      return `<section class="ask-qs${isAnswered(v) ? " done" : ""}" data-qid="${esc(x.id)}">
        <div class="ask-q">${qs.length > 1 ? `<span class="ask-num">${qi + 1}/${qs.length}</span>` : ""}<div class="md ask-md">${md(x.question || "", a.name, `${q.askedAt || ""}:${x.id}`)}</div>${x.multi ? '<span class="muted small"> · 여러 개 선택</span>' : ""}</div>
        <div class="ask-opts">${x.options.map(opt).join("")}</div>
        ${unsupported ? "" : `<input class="ask-text" data-q="${esc(x.id)}" placeholder="직접 답하기 (기타)" enterkeyhint="${enterSends() ? "send" : "done"}" value="${esc(v.text || "")}">`}</section>`;
    }).join("");
    const doneCount = qs.filter((x) => isAnswered(sel.get(x.id))).length;
    // No parsed options: never label it 권한 요청 (that needs choices); say the choices were not read and point to the terminal.
    const noOpts = qs.every((x) => !x.options.length);
    const title = (q.askKind === "permission" || a.askKind === "permission") && !noOpts ? "권한 요청" : "질문";
    H("#ask-card").innerHTML = `<div class="ask-head"><span class="ask-tag">${title}${qs.length > 1 ? ` ${qs.length}개` : ""}</span><span class="muted small">${q.askedAt ? `${ago(q.askedAt)}부터` : ""}</span><span class="spacer"></span><button data-act="focus" data-name="${n}">터미널 열기</button></div>
      ${sections}
      ${noOpts ? `<div class="ask-noopts">선택지를 읽지 못했습니다. 선택지가 있는 질문이면 <button data-act="focus" data-name="${n}">터미널 열기</button>로 확인해 답하세요. 직접 입력으로 답할 수도 있습니다.</div>` : ""}
      ${unsupported ? `<div class="muted small">이 질문은 대시보드에서 답할 수 없습니다. 터미널을 열어 답하세요.</div>` : `<div class="ask-submit"><span class="muted small" id="ask-progress">${doneCount}/${qs.length} 답함</span><button class="primary" data-act="ask-submit" ${doneCount === qs.length ? "" : "disabled"}>${qs.length > 1 ? "모두 답하기" : "답하기"}</button></div>`}`;
    enhanceAskCard(a.name);
  }
  // Same renderer as chat messages: path links (inline code + plain text), skill chips, rich blocks;
  // linked image paths also get a small thumbnail (click → viewer).
  async function enhanceAskCard(agent) {
    const card = $("#ask-card");
    if (!card.querySelector(".md:not([data-plchk])")) return;
    const skills = () => (cmdCache.has(agent) ? highlightSkills(card, agent) : loadCommands(agent).then(() => highlightSkills(card, agent)));
    const p = Promise.all([linkifyPaths(card, agent), linkifyPlainPaths(card, agent)]);
    skills();
    enhanceRich(card);
    await p;
    for (const a of card.querySelectorAll("a.pathlink:not([data-thumb])")) {
      a.dataset.thumb = "1";
      const pth = a.dataset.openPath;
      if (a.dataset.dir || !/\.(png|jpe?g|gif|webp|svg)$/i.test(pth)) continue;
      const img = document.createElement("img");
      img.className = "ask-thumb"; img.loading = "lazy"; img.alt = pth; img.title = `${pth}\n누르면 크게 보기`;
      img.src = FIXTURE ? "icons/icon-192.png" : fileUrl(agent, pth, agentBy(agent)?.ask?.askedAt || Date.now());
      img.dataset.viewPath = pth;
      img.onerror = () => img.remove();
      // A strip under the text, so the sentence is not split by the image.
      const host = a.closest(".ask-md") || a.parentElement;
      let strip = host.querySelector(":scope > .ask-thumbs");
      if (!strip) { strip = document.createElement("div"); strip.className = "ask-thumbs"; host.append(strip); }
      strip.append(img);
    }
  }
  // Text typed for a question: update state without re-rendering (keeps focus), then refresh the submit gate.
  function askTextChanged(input) {
    const sel = ov.askSel.answers, id = input.dataset.q, t = input.value;
    if (t.trim()) sel.set(id, { text: t }); else if (sel.get(id)?.text != null) sel.delete(id);
    const a = agentBy(ov.name), qs = a?.ask ? askQuestions(a.ask) : [];
    const done = qs.filter((x) => isAnswered(sel.get(x.id))).length;
    $("#ask-progress") && ($("#ask-progress").textContent = `${done}/${qs.length} 답함`);
    const btn = $('#ask-card [data-act="ask-submit"]'); if (btn) btn.disabled = done !== qs.length;
    $$("#ask-card .ask-qs").forEach((s) => s.classList.toggle("done", isAnswered(sel.get(s.dataset.qid))));
    $$(`#ask-card .ask-qs[data-qid="${CSS.escape(id)}"] .ask-opt.on`).forEach((b) => t.trim() && b.classList.remove("on"));
  }
  function askPick(qid, i) {
    const a = agentBy(ov.name), x = a?.ask && askQuestions(a.ask).find((y) => y.id === qid);
    if (!x) return;
    const sel = ov.askSel.answers, cur = sel.get(qid) || {};
    if (x.multi) {
      const set = new Set(cur.optionIndices || []);
      set.has(i) ? set.delete(i) : set.add(i);
      set.size ? sel.set(qid, { optionIndices: [...set].sort((p, q) => p - q) }) : sel.delete(qid);
    } else sel.set(qid, { optionIndex: i });
    renderAskCard();
  }
  async function submitAsk() {
    const a = agentBy(ov.name);
    if (!a?.ask) return;
    const qs = askQuestions(a.ask), sel = ov.askSel.answers;
    if (!qs.every((x) => isAnswered(sel.get(x.id)))) return toast("모든 질문에 답하세요");
    const answers = qs.map((x) => { const v = sel.get(x.id); return v.text?.trim() ? { id: x.id, text: v.text.trim() } : { id: x.id, ...v }; });
    await answerAsk({ answers });
  }
  // ---------- answer jobs: POST /answer → 202 {jobId}; GET /api/answers/{jobId} until done/failed ----------
  // A failed poll never reads as "failed": we re-check the agent, since the answer may well have landed.
  const answerJobs = new Map(); // agent name → {state: running|done|failed|unknown, step, k, n, verified, reason, body}
  const ANS_STEP_KO = { queued: "대기", focusing: "창 여는 중", selecting: "선택하는 중", typing: "입력하는 중", submitting: "제출하는 중", verifying: "반영 확인 중" };
  function answerJobHtml(j) {
    const prog = j.n ? `${Math.min(j.k || 0, j.n)}/${j.n}` : "";
    const bar = j.n ? `<div class="aj-bar"><span style="width:${Math.round(100 * Math.min(j.k || 0, j.n) / j.n)}%"></span></div>` : "";
    if (j.state === "running") return `<div class="aj running"><b>답 넣는 중 ${prog}…</b><span class="muted small">${esc(ANS_STEP_KO[j.step] || j.step || "")}</span>${bar}<div class="muted small">끝날 때까지 다른 조작은 잠시 막아 둡니다</div></div>`;
    if (j.state === "done") return `<div class="aj done"><b>${j.verified === false ? "답을 넣었습니다 · 반영은 확인하지 못함" : "답을 넣었습니다 ✓"}</b>${prog ? `<span class="muted small">${prog}</span>` : ""}${j.verified === false ? `<div class="muted small">터미널에서 한 번 확인해 주세요.</div><div class="aj-act"><button data-act="focus" data-name="${esc(j.name)}">터미널 열기</button><button data-act="aj-close">닫기</button></div>` : ""}</div>`;
    if (j.state === "unknown") return `<div class="aj warn"><b>진행 상태를 확인하지 못했습니다</b><div class="muted small">답이 들어갔을 수 있습니다. 다시 보내기 전에 터미널에서 확인하세요.${j.reason ? ` (${esc(j.reason)})` : ""}</div><div class="aj-act"><button data-act="focus" data-name="${esc(j.name)}">터미널 열기</button><button data-act="aj-close">닫기</button></div></div>`;
    return `<div class="aj failed"><b>답을 넣지 못했습니다${prog ? ` (${prog}에서 멈춤)` : ""}</b><div class="muted small">${esc(j.reason || "이유 없음")}</div><div class="aj-act"><button data-act="view" data-v="terminal">${t_("터미널 보기")}</button><button class="primary" data-act="aj-retry">다시 시도</button><button data-act="focus" data-name="${esc(j.name)}">터미널 열기</button><button data-act="aj-close">닫기</button></div></div>`;
  }
  const normJob = (r) => ({ state: r.state || r.status || "running", step: r.step || r.phase, k: r.k ?? r.done ?? r.progress?.done, n: r.n ?? r.total ?? r.progress?.total, verified: r.verified, reason: r.reason || r.error });
  function setJob(name, j) { answerJobs.set(name, { ...answerJobs.get(name), ...j, name }); renderAskCard(); render(); }
  function endJob(name, ms) {
    setTimeout(() => { const j = answerJobs.get(name); if (j && j.state === "done") { answerJobs.delete(name); renderAskCard(); render(); } }, ms);
    if (!FIXTURE) refresh();
  }
  async function answerAsk(body) {
    const name = ov.name;
    if (answerJobs.get(name)?.state === "running") return;
    const n = body.answers?.length || 1;
    setJob(name, { state: "running", step: "queued", k: 0, n, body, reason: null, verified: null, askedAt: agentBy(name)?.ask?.askedAt ?? null });
    let r;
    try { r = await api("POST", `/api/agents/${enc(name)}/answer`, body, { quiet: true }); }
    catch (e) {
      answerJobs.delete(name);
      // No /answer (or it cannot drive this prompt): fall back to answering in the terminal.
      if ([404, 405, 409, 501].includes(e.status)) ov.answerUnsupported.add(name);
      else toast(`답을 보내지 못했습니다: ${e.message}`);
      renderAskCard(); render(); return;
    }
    // Older servers answer synchronously.
    if (!r?.jobId) { setJob(name, { state: r?.ok === false ? "failed" : "done", k: n, verified: r?.verified, reason: r?.error }); return endJob(name, r?.verified === false ? 600000 : 2500); }
    let misses = 0;
    for (;;) {
      await new Promise((ok) => setTimeout(ok, FIXTURE ? 300 : 700));
      let j;
      try { j = normJob(await api("GET", `/api/answers/${enc(r.jobId)}`, undefined, { quiet: true })); misses = 0; }
      catch (e) {
        if (++misses < 5 && e.status !== 404) continue;
        // Lost track of the job: decide from the agent itself rather than calling it a failure.
        if (!FIXTURE) await refresh();
        const still = kindOf(agentBy(name) || {}) === "asking";
        setJob(name, still ? { state: "unknown", reason: e.message } : { state: "done", k: n, verified: null });
        return still ? null : endJob(name, 2500);
      }
      if (j.state === "running" || j.state === "queued") { setJob(name, { ...j, state: "running" }); continue; }
      if (j.state === "done" || j.state === "ok" || j.state === "succeeded") { setJob(name, { ...j, state: "done" }); return endJob(name, j.verified === false ? 600000 : 2500); }
      setJob(name, { ...j, state: "failed" });
      if (!FIXTURE) refresh();
      return;
    }
  }


  // ---------- usage (v27): agent.usage ----------
  // {context: {used, limit, pct?, compactions}, session: {turns, input, output, cacheRead, cacheWrite, cost, startedAt},
  //  model, thinking, account: {provider, account, windows: [{label: "5h"|"7d", pct, resetsAt}]}}
  // Every part is optional; a missing part is simply not shown. Cache time left comes from agent.cache.
  const fmtTok = (n) => (n == null ? "–" : n >= 1e6 ? `${+(n / 1e6).toFixed(n % 1e6 ? 1 : 0)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}K` : String(n));
  const fmtCost = (c) => (c == null ? "" : c >= 100 ? `$${c.toFixed(0)}` : c >= 1 ? `$${c.toFixed(2)}` : `$${c.toFixed(3)}`);
  const ubar = (pct) => { const p = Math.max(0, Math.min(100, +pct || 0)); return `<span class="ubar"><i class="${p >= 90 ? "hi" : p >= 70 ? "mid" : ""}" style="width:${p.toFixed(1)}%"></i></span>`; };
  const leftTo = (t) => { const m = (t - nowTick()) / 60000; return m <= 0 ? "곧 초기화" : `초기화까지 ${fmtMin(m) || "1분 미만"}`; };
  function usageHtml(a, compact = false) {
    const u = a.usage;
    // Phone: the sheet also carries the model line (the info list is not shown there), so it exists whenever a model is known.
    if (!u && !cacheExp(a) && !(compact && (a.model || a.agentType))) return "";
    const c = u?.context, ss = u?.session, ac = u?.account;
    const parts = [];
    if (c && (c.used != null || c.pct != null)) {
      const pct = c.pct ?? (c.limit ? (100 * c.used) / c.limit : null);
      parts.push(`<div class="u-row"><span class="u-k">컨텍스트</span>${pct != null ? ubar(pct) : ""}<span class="u-v${pct >= 90 ? " hi" : pct >= 70 ? " mid" : ""}">${pct != null ? `<b class="u-num">${pct.toFixed(1)}%</b>` : ""}${c.used != null ? ` · ${fmtTok(c.used)}${c.limit ? ` / ${fmtTok(c.limit)}` : ""}` : ""}</span></div>${c.compactions ? `<div class="u-sub muted small">${t_(`컴팩션 ${c.compactions}회`)}${c.lastCompactionAt ? ` · 마지막 ${ago(c.lastCompactionAt)}` : ""}</div>` : ""}`);
    }
if (compact && a.agentType) parts.push(`<div class="u-row"><span class="u-k">모델</span><span class="u-v"><button type="button" class="model-btn" data-model-pick="${esc(a.name)}">${esc(u?.model || a.model || L2("알 수 없음", "unknown"))}${u?.thinking ? ` · thinking ${esc(u.thinking)}` : ""} <span class="muted">▾</span></button></span></div>`);
    if (ss) {
      const tok = [["입력 토큰", ss.input], ["출력 토큰", ss.output], ["캐시 읽기", ss.cacheRead], ["캐시 쓰기", ss.cacheWrite]].filter(([, v]) => v != null);
      parts.push(`<div class="u-row"><span class="u-k">이 세션</span><span class="u-v">${[ss.turns != null ? `턴 ${ss.turns}` : "", ss.cost != null ? `<b class="u-num">${fmtCost(ss.cost)}</b>` : "", ss.startedAt ? `${fmtMin((nowTick() - ss.startedAt) / 60000) || "1분 미만"} 경과` : ""].filter(Boolean).join(" · ")}</span></div>${tok.length ? `<div class="u-sub u-tok">${tok.map(([k, v]) => `<span><span class="muted">${k}</span> ${fmtTok(v)}</span>`).join("")}</div>` : ""}`);
    }
    if (ac && (ac.windows?.length || ac.provider)) {
      parts.push(`<div class="u-row"><span class="u-k">계정</span><span class="u-v">${esc([ac.provider ? provLabel(ac.provider) : "", ac.account].filter(Boolean).join(" · "))}</span></div>${(ac.windows || []).map((w) => `<div class="u-row u-win"${w.name || w.recordedAt ? ` title="${esc([w.name, w.recordedAt ? `기록 ${hhmm(w.recordedAt)}` : ""].filter(Boolean).join(" · "))}"` : ""}><span class="u-k muted">${esc(w.label || "")}</span>${ubar(w.pct)}<span class="u-v${w.pct >= 90 ? " hi" : w.pct >= 70 ? " mid" : ""}">${w.pct != null ? `<b class="u-num">${Math.round(w.pct)}%</b>` : ""}${w.resetsAt ? ` <span class="muted">${leftTo(w.resetsAt)}</span>` : ""}</span></div>`).join("")}`);
    }
    const exp = cacheExp(a);
    if (compact && exp && cacheState(a) !== "cold") parts.push(`<div class="u-row"><span class="u-k">캐시</span><span class="u-v"><span class="cd" data-exp="${exp}" data-basis="${esc(cacheBasis(a))}"></span> <span class="muted small">남음</span></span></div>`);
    if (!parts.length) return "";
    if (!compact) return `<div class="usage"><h4>사용량</h4>${parts.join("")}</div>`;
    // Phone summary line: context %, session cost, the tightest account window.
    const pct = c ? c.pct ?? (c.limit ? (100 * c.used) / c.limit : null) : null;
    const w = (ac?.windows || []).filter((x) => x.pct != null).sort((p, q) => q.pct - p.pct)[0];
    const sum = [pct != null ? `컨텍스트 ${pct.toFixed(0)}%` : "", ss?.cost != null ? fmtCost(ss.cost) : "", w ? `${w.label} ${Math.round(w.pct)}%` : ""].filter(Boolean).join(" · ") || (cacheExp(a) ? `캐시 ${fmtMin((cacheExp(a) - nowTick()) / 60000) || "1분 미만"} 남음` : "");
    return `<summary><b>사용량</b><span class="muted">${esc(sum)}</span></summary><div class="usage">${parts.join("")}</div>`;
  }
  // ---------- hourly usage chart (v43): GET /api/agents/{name}/usage/hourly ----------
  // {tokens: bool, hours: [{t (hour start, ms or ISO), input, output, cacheRead, cacheWrite, cost, userMsgs, toolCalls, turns}]}
  // Hours with no activity may be omitted: they draw as a faint floor line, not a 0 bar. tokens:false (the harness
  // does not record token usage) → a note and the turn/message strip only. Theme tokens only: --c1 + neutral --fg mixes,
  // never the turn / error / ask colours.
  const fmtK = (n) => (n == null ? "–" : n >= 1e6 ? `${+(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${+(n / 1e3).toFixed(1)}k` : String(Math.round(n)));
  const UH_SERIES = [["output", "출력"], ["input", "입력"], ["cacheRead", "캐시 읽기"], ["cacheWrite", "캐시 쓰기"]];
  const uh = { name: null, data: null, at: 0, range: localStorage.getItem("adash.uh.range") || "24h", off: new Set(JSON.parse(localStorage.getItem("adash.uh.off") || "[]")), el: null, w: 0, loading: false };
  function fixtureHourly(name) {
    const a = fixtureData.agents.find((x) => x.name === name);
    let seed = [...name].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7);
    const rnd = () => ((seed = (seed * 1103515245 + 12345) >>> 0) / 4294967296);
    const tokens = !(a && /antigravity|agy/.test(a.agentType || "")), model = a?.model || (tokens ? "claude-opus-5-5" : "");
    const H = 3600000, end = Math.floor(now() / H) * H, hours = [];
    for (let k = 47; k >= 0; k--) {
      const hourStart = end - k * H, idle = rnd() < (k > 30 ? 0.55 : 0.22);
      const work = idle ? 0 : 0.3 + rnd() * (k < 6 ? 1.4 : 0.9);
      const z = (v) => (tokens ? v : null);
      const output = Math.round(work ? 4000 + 26000 * work : 0), input = Math.round(work ? 9000 + 60000 * work * rnd() : 0), cacheRead = Math.round(work ? 150000 + 900000 * work : 0), cacheWrite = Math.round(work ? 8000 + 50000 * rnd() : 0);
      const sub = work && rnd() < 0.3 ? Math.round((input + output) * 0.4) : 0;
      hours.push({ hourStart, input: z(input), cacheRead: z(cacheRead), cacheWrite: z(cacheWrite), output: z(output), costUsd: z(+((input * 15 + output * 75 + cacheRead * 1.5 + cacheWrite * 18.75) / 1e6).toFixed(3)), turns: work ? 1 + Math.floor(work * 8) : 0, userMsgs: work ? Math.floor(rnd() * 4) : 0, toolCalls: work ? Math.floor(work * 30 * rnd()) : 0, subagentTokens: z(sub), model: work ? model : "" });
    }
    const sum = (k) => (tokens || !["input", "cacheRead", "cacheWrite", "output", "costUsd", "subagentTokens"].includes(k) ? hours.reduce((s, h) => s + (h[k] || 0), 0) : null);
    return { hours, total: Object.fromEntries(["input", "cacheRead", "cacheWrite", "output", "costUsd", "turns", "userMsgs", "toolCalls", "subagentTokens"].map((k) => [k, sum(k)])), sessionStartedAt: end - 40 * H, tokens, from: end - 47 * H, to: end + H };
  }
  const uhRangeH = (r) => ({ "6h": 6, "24h": 24, "48h": 48 })[r] || 0;
  async function uhLoad(name, force) {
    if (!force && uh.name === name && uh.data && Date.now() - uh.at < 300000) return;
    uh.name = name; uh.loading = true;
    try {
      // Always 336 h (the server maximum); the 6h/24h/48h/전체 toggle slices locally, so switching needs no refetch.
      const r = await api("GET", `/api/agents/${enc(name)}/usage/hourly?hours=336`, undefined, { quiet: true });
      if (uh.name !== name) return;
      const tokens = r?.tokens !== false;
      // Every hour comes back (zeros included); an hour with no turns, messages, tools or tokens counts as empty.
      const list = (r?.hours || []).map((h) => ({ t: h.hourStart, input: h.input, output: h.output, cacheRead: h.cacheRead, cacheWrite: h.cacheWrite, cost: h.costUsd, turns: h.turns || 0, userMsgs: h.userMsgs || 0, toolCalls: h.toolCalls || 0, sub: h.subagentTokens, model: h.model || "" }))
        .filter((h) => Number.isFinite(h.t) && (h.turns || h.userMsgs || h.toolCalls || (tokens && (h.input || h.output || h.cacheRead || h.cacheWrite))));
      uh.data = { tokens, hours: list, startedAt: r?.sessionStartedAt || null };
    } catch (e) { if (uh.name === name) uh.data = { error: /404|not found|no session/i.test(e.message) ? L2("세션 파일이 없어 시간별 기록을 만들 수 없습니다", "No session file, so there is no hourly record") : e.message, hours: [] }; }
    uh.at = Date.now(); uh.loading = false;
    if (uh.name === name) uhRender();
  }
  function uhHost() {
    if (!uh.el) {
      uh.el = document.createElement("div"); uh.el.className = "uh"; uh.el.setAttribute("data-no-i18n", "");
      uh.el.addEventListener("click", (e) => {
        const rb = e.target.closest("[data-uh-range]"); if (rb) { uh.range = rb.dataset.uhRange; localStorage.setItem("adash.uh.range", uh.range); uhRender(); return; }
        const lb = e.target.closest("[data-uh-s]"); if (lb) { const k = lb.dataset.uhS; uh.off.has(k) ? uh.off.delete(k) : uh.off.add(k); localStorage.setItem("adash.uh.off", JSON.stringify([...uh.off])); uhRender(); }
      });
      uh.el.addEventListener("mousemove", uhTip); uh.el.addEventListener("mouseleave", () => { const tip = uh.el.querySelector(".uh-tip"); if (tip) tip.hidden = true; });
      new ResizeObserver(() => { if (uh.el.clientWidth && Math.abs(uh.el.clientWidth - uh.w) > 4) uhRender(); }).observe(uh.el);
    }
    return uh.el;
  }
  function uhSlots() {
    const H = 3600000, hrs = uh.data?.hours || [];
    if (!hrs.length) return [];
    const end = Math.floor(nowTick() / H) * H, first = Math.min(uh.data.startedAt ? Math.floor(uh.data.startedAt / H) * H : Infinity, ...hrs.map((h) => h.t));
    const n = uhRangeH(uh.range) || Math.min(336, Math.max(1, Math.round((end - first) / H) + 1));
    const by = new Map(hrs.map((h) => [Math.floor(h.t / H) * H, h]));
    return Array.from({ length: n }, (_, i) => { const t = end - (n - 1 - i) * H; return { t, h: by.get(t) || null }; });
  }
  function uhRender() {
    const el = uh.el; if (!el || !el.isConnected) return;
    const d = uh.data, w = Math.max(220, el.clientWidth || 300); uh.w = w;
    const seg = `<div class="seg uh-range">${["6h", "24h", "48h", "all"].map((r) => `<button type="button" data-uh-range="${r}" class="${uh.range === r ? "on" : ""}">${r === "all" ? L2("전체", "All") : r}</button>`).join("")}</div>`;
    const head = `<div class="uh-head"><b>${L2("시간별 사용량", "Hourly usage")}</b>${seg}</div>`;
    if (!d) { el.innerHTML = `${head}<p class="muted small">${L2("불러오는 중…", "Loading…")}</p>`; return; }
    if (d.error) { el.innerHTML = `${head}<p class="muted small">${esc(L2("불러오지 못함: ", "Could not load: ") + d.error)}</p>`; return; }
    const slots = uhSlots();
    if (!slots.length || !slots.some((s) => s.h)) { el.innerHTML = `${head}<p class="muted small">${L2("이 기간에 기록된 사용량이 없습니다", "No usage recorded in this period")}</p>`; return; }
    const tokens = d.tokens, on = UH_SERIES.filter(([k]) => !uh.off.has(k));
    const padL = 34, padR = tokens ? 40 : 8, plotW = w - padL - padR, n = slots.length, bw = plotW / n, gap = Math.min(2, bw * 0.2);
    const BH = tokens ? 110 : 0, top = 8, stripY = top + BH + (tokens ? 14 : 0), stripH = 14, H = stripY + stripH * 2 + 20;
    const x = (i) => padL + i * bw;
    let svg = "";
    // axis labels: every few hours
    const step = Math.max(1, Math.ceil(n / Math.max(2, Math.floor(plotW / 46))));
    const tick = (t) => { const dt = new Date(t); return dt.getHours() === 0 ? `${dt.getMonth() + 1}/${dt.getDate()}` : `${dt.getHours()}${window.ADASH_LANG === "en" ? "h" : "시"}`; };
    slots.forEach((s, i) => { if ((n - 1 - i) % step === 0) svg += `<text class="uh-ax" x="${(x(i) + bw / 2).toFixed(1)}" y="${H - 4}" text-anchor="middle">${tick(s.t)}</text>`; });
    if (tokens) {
      const tot = (h) => on.reduce((a, [k]) => a + (h?.[k] || 0), 0);
      const maxT = Math.max(1, ...slots.map((s) => tot(s.h)));
      const maxC = Math.max(0.001, ...slots.map((s) => s.h?.cost || 0));
      const y = (v) => top + BH - (v / maxT) * BH, yc = (v) => top + BH - (v / maxC) * BH;
      svg += `<line class="uh-grid" x1="${padL}" x2="${w - padR}" y1="${top}" y2="${top}"/><line class="uh-grid" x1="${padL}" x2="${w - padR}" y1="${top + BH / 2}" y2="${top + BH / 2}"/>`;
      svg += `<text class="uh-ax" x="${padL - 4}" y="${top + 4}" text-anchor="end">${fmtK(maxT)}</text><text class="uh-ax" x="${padL - 4}" y="${top + BH}" text-anchor="end">0</text>`;
      svg += `<text class="uh-ax uh-cost-ax" x="${w - padR + 4}" y="${top + 4}">${fmtCost(maxC)}</text><text class="uh-ax uh-cost-ax" x="${w - padR + 4}" y="${top + BH}">$0</text>`;
      slots.forEach((s, i) => {
        if (!s.h) { svg += `<line class="uh-floor" x1="${(x(i) + gap / 2).toFixed(1)}" x2="${(x(i) + bw - gap / 2).toFixed(1)}" y1="${top + BH - 0.5}" y2="${top + BH - 0.5}"/>`; return; }
        let acc = 0;
        for (const [k] of on) { const v = s.h[k] || 0; if (!v) continue; const y0 = y(acc), y1 = y(acc + v); svg += `<rect class="uh-s-${k}" x="${(x(i) + gap / 2).toFixed(1)}" y="${y1.toFixed(1)}" width="${Math.max(1, bw - gap).toFixed(1)}" height="${Math.max(0.5, y0 - y1).toFixed(1)}"/>`; acc += v; }
      });
      const pts = slots.map((s, i) => (s.h?.cost != null ? [x(i) + bw / 2, yc(s.h.cost)] : null));
      let dpath = "", pen = false; for (const p of pts) { if (!p) { pen = false; continue; } dpath += `${pen ? "L" : "M"}${p[0].toFixed(1)} ${p[1].toFixed(1)}`; pen = true; }
      svg += `<path class="uh-cost" d="${dpath}"/>${pts.filter(Boolean).map((p) => `<circle class="uh-cost-pt" cx="${p[0].toFixed(1)}" cy="${p[1].toFixed(1)}" r="${bw > 10 ? 2 : 1.4}"/>`).join("")}`;
    }
    // strip: my messages (dots) and tool calls (intensity)
    const maxTool = Math.max(1, ...slots.map((s) => s.h?.toolCalls || 0)), maxTurn = Math.max(1, ...slots.map((s) => s.h?.turns || 0));
    svg += `<text class="uh-ax" x="${padL - 4}" y="${stripY + 10}" text-anchor="end">${tokens ? L2("도구", "tools") : L2("턴", "turns")}</text><text class="uh-ax" x="${padL - 4}" y="${stripY + stripH + 10}" text-anchor="end">${L2("나", "me")}</text>`;
    slots.forEach((s, i) => {
      const xx = (x(i) + gap / 2).toFixed(1), ww = Math.max(1, bw - gap).toFixed(1);
      const v = tokens ? s.h?.toolCalls || 0 : s.h?.turns || 0, mx = tokens ? maxTool : maxTurn;
      svg += v ? `<rect class="uh-tool" x="${xx}" y="${stripY}" width="${ww}" height="${stripH - 3}" style="opacity:${(0.18 + 0.82 * (v / mx)).toFixed(2)}"/>` : `<line class="uh-floor" x1="${xx}" x2="${(+xx + +ww).toFixed(1)}" y1="${stripY + stripH - 3.5}" y2="${stripY + stripH - 3.5}"/>`;
      const m = s.h?.userMsgs || 0;
      if (m) { const r = Math.min(3, bw / 4 + 0.6); for (let j = 0; j < Math.min(m, 3); j++) svg += `<circle class="uh-msg" cx="${(x(i) + bw / 2).toFixed(1)}" cy="${(stripY + stripH + 5 + j * 0).toFixed(1)}" r="${(r + j * 0.6).toFixed(1)}"/>`; }
    });
    const legend = tokens ? `<div class="uh-legend">${UH_SERIES.map(([k, ko]) => `<button type="button" data-uh-s="${k}" class="uh-chip${uh.off.has(k) ? " off" : ""}" aria-pressed="${!uh.off.has(k)}"><i class="uh-sw uh-s-${k}"></i>${esc(window.ADASH_LANG === "en" ? { output: "output", input: "input", cacheRead: "cache read", cacheWrite: "cache write" }[k] : ko)}</button>`).join("")}<span class="uh-chip static"><i class="uh-sw uh-sw-cost"></i>${L2("비용 (오른쪽 축)", "cost (right axis)")}</span></div>` : `<p class="muted small uh-note">${L2("토큰 정보 없음 · 이 하네스는 토큰 사용량을 기록하지 않음", "No token data · this harness does not record token usage")}</p>`;
    const sum = slots.reduce((a, s) => (s.h ? { tok: a.tok + UH_SERIES.reduce((b, [k]) => b + (s.h[k] || 0), 0), cost: a.cost + (s.h.cost || 0), msgs: a.msgs + (s.h.userMsgs || 0), tools: a.tools + (s.h.toolCalls || 0), turns: a.turns + (s.h.turns || 0) } : a), { tok: 0, cost: 0, msgs: 0, tools: 0, turns: 0 });
    const sumTxt = tokens ? L2(`토큰 ${fmtK(sum.tok)} · ${fmtCost(sum.cost)} · 내 메시지 ${sum.msgs} · 도구 ${sum.tools}`, `${fmtK(sum.tok)} tokens · ${fmtCost(sum.cost)} · ${sum.msgs} my messages · ${sum.tools} tool calls`) : L2(`턴 ${sum.turns} · 내 메시지 ${sum.msgs}`, `${sum.turns} turns · ${sum.msgs} my messages`);
    el.innerHTML = `${head}${legend}<svg class="uh-svg" width="${w}" height="${H}" viewBox="0 0 ${w} ${H}" role="img" aria-label="${esc(L2("시간별 사용량", "Hourly usage"))}">${svg}<rect class="uh-hover" x="0" y="0" width="0" height="${stripY + stripH * 2}" hidden/></svg><div class="uh-sum muted small">${esc(sumTxt)}</div><div class="uh-tip" hidden></div>`;
    el._geo = { padL, bw, n, slots, tokens, h: stripY + stripH * 2 };
  }
  function uhTip(e) {
    const g = uh.el._geo, svg = uh.el.querySelector(".uh-svg"), tip = uh.el.querySelector(".uh-tip"); if (!g || !svg || !tip) return;
    const r = svg.getBoundingClientRect(), i = Math.floor((e.clientX - r.left - g.padL) / g.bw), hov = svg.querySelector(".uh-hover");
    if (i < 0 || i >= g.n || e.clientY - r.top > g.h + 4) { tip.hidden = true; hov.setAttribute("hidden", ""); return; }
    const s = g.slots[i], dt = new Date(s.t), lbl = `${dt.toLocaleDateString(window.ADASH_LANG === "en" ? "en-US" : "ko-KR", { month: "numeric", day: "numeric" })} ${String(dt.getHours()).padStart(2, "0")}:00–${String((dt.getHours() + 1) % 24).padStart(2, "0")}:00`;
    const h = s.h, rows = !h ? [[L2("활동 없음", "no activity"), ""]] : [
      ...(g.tokens ? [[L2("출력", "output"), fmtK(h.output)], [L2("입력", "input"), fmtK(h.input)], [L2("캐시 읽기", "cache read"), fmtK(h.cacheRead)], [L2("캐시 쓰기", "cache write"), fmtK(h.cacheWrite)], [L2("비용", "cost"), fmtCost(h.cost) || "–"]] : []),
      ...(g.tokens && h.sub ? [[L2("서브에이전트 몫", "subagents' share"), fmtK(h.sub)]] : []),
      [L2("턴", "turns"), h.turns ?? "–"], [L2("내 메시지", "my messages"), h.userMsgs ?? 0], [L2("도구 호출", "tool calls"), h.toolCalls ?? 0], ...(h.model ? [[L2("모델", "model"), h.model]] : [])];
    tip.innerHTML = `<b>${esc(lbl)}</b>${rows.map(([k, v]) => `<div><span class="muted">${esc(k)}</span><span>${esc(String(v))}</span></div>`).join("")}`;
    tip.hidden = false;
    hov.removeAttribute("hidden"); hov.setAttribute("x", (g.padL + i * g.bw).toFixed(1)); hov.setAttribute("width", g.bw.toFixed(1));
    const host = uh.el.getBoundingClientRect(), tw = tip.offsetWidth, cx = e.clientX - host.left;
    tip.style.left = `${Math.max(0, Math.min(host.width - tw, cx + 12 > host.width - tw ? cx - tw - 12 : cx + 12))}px`;
    tip.style.top = `${Math.max(0, e.clientY - host.top - tip.offsetHeight - 8)}px`;
  }
  // Mount the chart under the usage section (desktop info pane or the phone usage sheet) and keep it loaded.
  function uhMount(a, parent) {
    if (!a || !parent) return;
    const el = uhHost();
    const after = parent.querySelector(".usage") || null;
    if (el.parentNode !== parent || (after && el.previousElementSibling !== after)) { after ? after.after(el) : parent.append(el); uhRender(); }
    if (uh.name !== a.name) { uh.data = null; uhRender(); }
    uhLoad(a.name);
  }
  // ---------- model / effort picker (v43): GET /api/agents/{n}/models, POST /api/agents/{n}/model {model?, effort?} ----------
  // GET → {harness, current: {model, effort}, models: [{id, label?, provider?}], efforts: [..], canSetModel, canSetEffort, how?}
  // Applies only when the agent is not working (no queued "apply later"); success = the state shows the new values.
  const mp = { name: null, data: null, model: null, effort: null, q: "", busy: false, err: "" };
  function modelPopEl() {
    let el = $("#model-pop");
    if (!el) {
      el = document.createElement("div"); el.id = "model-pop"; el.className = "pop model-pop"; el.hidden = true; el.setAttribute("data-no-i18n", "");
      document.body.appendChild(el);
      el.addEventListener("input", (e) => { if (e.target.id === "mp-q") { mp.q = e.target.value; renderModelPop(true); } });
      el.addEventListener("click", (e) => {
        const m = e.target.closest("[data-mp-model]"); if (m && !m.disabled) { mp.model = m.dataset.mpModel; return renderModelPop(); }
        const f = e.target.closest("[data-mp-effort]"); if (f && !f.disabled) { mp.effort = f.dataset.mpEffort; return renderModelPop(); }
        if (e.target.closest("#mp-apply")) return applyModel();
        if (e.target.closest("#mp-restart")) { applyModel.restart = true; return applyModel(); }
        if (e.target.closest("#mp-close")) return closeModelPop();
      });
    }
    return el;
  }
  const closeModelPop = () => { const el = $("#model-pop"); if (el) el.hidden = true; document.body.classList.remove("sheet-open"); mp.name = null; };
  async function openModelPop(name, btn) {
    const el = modelPopEl();
    Object.assign(mp, { name, data: null, model: null, effort: null, q: "", busy: false, err: "" });
    el.hidden = false; document.body.classList.toggle("sheet-open", isMobile());
    mp.anchor = btn;
    renderModelPop();
    try { mp.data = await api("GET", `/api/agents/${enc(name)}/models`, undefined, { quiet: true }); }
    catch (e) { mp.err = e.message; }
    if (mp.name !== name) return;
    mp.model = mp.data?.current?.model || null; mp.effort = mp.data?.current?.effort || null;
    renderModelPop();
  }
  // Effort steps follow the model: models[].efforts of the picked model (falls back to the top-level list).
  const mpEfforts = () => { const d = mp.data; const m = (d?.models || []).find((x) => x.id === mp.model); return m && Array.isArray(m.efforts) ? m.efforts : d?.efforts || []; };
  function renderModelPop(keepFocus) {
    const el = $("#model-pop"); if (!el || el.hidden) return;
    const a = agentBy(mp.name), d = mp.data;
    const head = `<div class="mp-head"><b>${esc(L2("모델", "Model"))}</b><span class="muted small">${esc(mp.name || "")}${d?.harness ? ` · ${esc(d.harness)}` : ""}</span><span class="spacer"></span><button type="button" id="mp-close" aria-label="${esc(L2("닫기", "Close"))}">✕</button></div>`;
    if (!d) { el.innerHTML = head + `<p class="muted small">${esc(mp.err ? L2(`불러오지 못함: ${mp.err}`, `Could not load: ${mp.err}`) : L2("불러오는 중…", "Loading…"))}</p>`; if (mp.anchor) placePop(el, mp.anchor); return; }
    const working = a && (a.state === "working" || kindOf(a) === "working");
    const cur = d.current || {}, q = mp.q.trim().toLowerCase();
    const list = (d.models || []).filter((m) => !q || `${m.id} ${m.label || ""}`.toLowerCase().includes(q));
    const canM = d.canSetModel !== false, effs = mpEfforts(), canE = d.canSetEffort !== false && effs.length > 0;
    if (mp.effort && !effs.includes(mp.effort)) mp.effort = effs.includes(cur.effort) ? cur.effort : null;
    const changed = (mp.model && mp.model !== cur.model) || (mp.effort && mp.effort !== cur.effort);
    const off = (!canM && !canE) || working;
    const why = !canM && d.canSetEffort === false ? d.how || L2("이 하네스는 여기서 모델을 바꿀 수 없습니다", "This harness can't switch models from here")
      : working ? L2("작업 중에는 바꿀 수 없습니다. 작업이 끝난 뒤 다시 여세요", "Can't switch while it is working. Open this again when it finishes") : "";
    el.innerHTML = head + `
      ${(d.models || []).length ? `<input id="mp-q" type="search" placeholder="${esc(L2("모델 찾기", "Find a model"))}" value="${esc(mp.q)}" autocomplete="off"${canM && !working ? "" : " disabled"}>
      <div class="mp-list" role="listbox">${list.map((m) => `<button type="button" role="option" class="mp-m${m.id === mp.model ? " on" : ""}" data-mp-model="${esc(m.id)}" aria-selected="${m.id === mp.model}"${canM && !working ? "" : " disabled"}><span class="mp-id">${esc(m.label || m.id)}</span><span class="muted small">${esc(m.label && m.label !== m.id ? m.id : "")}</span>${m.id === cur.model ? `<span class="mp-cur">${esc(L2("현재", "current"))}</span>` : ""}</button>`).join("") || `<p class="muted small">${esc(L2("일치하는 모델 없음", "No matching model"))}</p>`}</div>` : ""}
      ${!canM && d.how && d.canSetEffort !== false ? `<p class="mp-why small">${esc(d.how)}</p>` : ""}
      ${canE ? `<div class="mp-eff"><span class="muted small">effort</span><div class="seg">${effs.map((x) => `<button type="button" data-mp-effort="${esc(x)}" class="${x === mp.effort ? "on" : ""}${x === cur.effort ? " cur" : ""}"${x === cur.effort ? ` title="${esc(L2("현재", "current"))}"` : ""}${working ? " disabled" : ""}>${esc(x)}</button>`).join("")}</div></div>`
        : d.canSetEffort !== false && mp.model ? `<p class="muted small">${esc(L2("이 모델은 effort 단계가 없습니다", "This model has no effort levels"))}</p>` : ""}
      ${d.persistsDefault && changed && !off ? `<p class="mp-warn small">${esc(L2("이 선택은 Claude Code 의 새 세션 기본값으로도 저장됩니다", "This choice is also saved as Claude Code's default for new sessions"))}</p>` : ""}
      ${why ? `<p class="mp-why small">${esc(why)}</p>` : ""}
      ${mp.err ? `<p class="mp-err small">${esc(mp.err)}</p>` : ""}${mp.restart ? `<p><button type="button" class="primary" id="mp-restart">${esc(L2("다시 시작 후 모델 적용", "Restart, then apply"))}</button></p>` : ""}
      <div class="mp-foot"><span class="muted small">${mp.busy ? esc(L2("적용 중… 하네스 확인을 기다리는 중", "Applying… waiting for the harness to confirm")) : ""}</span><span class="spacer"></span><button type="button" id="mp-apply" class="primary"${!changed || off || mp.busy ? " disabled" : ""}>${esc(mp.busy ? L2("적용 중…", "Applying…") : L2("적용", "Apply"))}</button></div>`;
    if (mp.anchor?.isConnected) placePop(el, mp.anchor); else if (!isMobile()) placePop(el, document.querySelector(`[data-model-pick="${CSS.escape(mp.name || "")}"]`) || el);
    if (keepFocus && el.querySelector("#mp-q")) { const i = el.querySelector("#mp-q"); i.focus(); i.setSelectionRange(i.value.length, i.value.length); }
  }
  async function applyModel() {
    const name = mp.name, d = mp.data, cur = d?.current || {};
    const body = {}; if (mp.model && mp.model !== cur.model) body.model = mp.model; if (mp.effort && mp.effort !== cur.effort) body.effort = mp.effort;
    if (!Object.keys(body).length) return;
    // after "이 세션은 omp 업데이트 전에 시작…": the same choice once more, after a restart
    if (applyModel.restart) { body.restartFirst = true; applyModel.restart = false; toast(L2(`${name} 다시 시작한 뒤 모델을 바꾸는 중…`, `Restarting ${name}, then switching…`), 20000); }
    mp.busy = true; mp.err = ""; mp.restart = false; renderModelPop();
    // The server waits for the harness to confirm (session record or screen line): 200 = switched.
    // 409 working/asking/draft/unsupported · 400 not in the list · 502 not confirmed → its {error} text.
    try { await api("POST", `/api/agents/${enc(name)}/model`, body, { quiet: true }); }
    catch (e) { if (mp.name !== name) return; mp.busy = false; mp.err = e.message; mp.restart = !!e.data?.restartable; renderModelPop(); toast(`${name}: ${e.message}`, 5000); return; }
    const label = (d.models || []).find((m) => m.id === body.model)?.label || body.model;
    toast(L2(`${name} · ${[label, body.effort && `effort ${body.effort}`].filter(Boolean).join(" · ")} 로 바뀜`, `${name} switched to ${[label, body.effort && `effort ${body.effort}`].filter(Boolean).join(" · ")}`), 3000);
    if (mp.name === name) closeModelPop();
    await refresh(); render();
  }
  function renderInfo() {
    const a = ov.name && agentBy(ov.name);
    let html;
    if (!a) html = ov.name ? `<p class="muted">${esc(ov.name)} 에이전트가 더 이상 없습니다.</p>` : `<p class="muted">선택된 에이전트 없음</p>`;
    else {
      const pr = a.progress, n = esc(a.name), tx = ov.tx;
html = `<dl class="kv"><dt>제목</dt><dd>${esc(a.title || "-")}</dd>
          <dt>하네스</dt><dd>${esc(a.agentType || "?")}${a.outdated ? ` <span class="od-mark" title="${esc(odTip(a.outdated))}">↑ ${esc(a.outdated.running || "")} → ${esc(a.outdated.installed || "")}</span> ${odPending.has(a.name) ? `<button type="button" data-od-cancel="${esc(a.name)}">${L2("예약 취소", "Cancel")}</button>` : `<button type="button" data-act="od-one" data-name="${esc(a.name)}">${L2("다시 시작", "Restart")}</button>`}` : ""}</dd>
          <dt>모델</dt><dd><button type="button" class="model-btn" data-model-pick="${n}" title="${esc(L2("모델·effort 바꾸기", "Change model / effort"))}">${esc([a.usage?.model || a.model || L2("알 수 없음", "unknown"), a.usage?.thinking ? `thinking ${a.usage.thinking}` : ""].filter(Boolean).join(" · "))} <span class="muted">▾</span></button></dd>
          <dt>상태</dt><dd>${esc(stateKo(a.state))} · ${fmtMin(sinceMin(a))}${a.snoozedUntil ? ` · ${L2("미룸", "snoozed")} ~${fmtUntil(a.snoozedUntil)}` : ""}</dd>
          ${cacheState(a) ? `<dt>캐시</dt><dd${cacheBasis(a) ? ` title="${esc(cacheBasis(a))}"` : ""}><span class="badge ${esc(cacheState(a))}">${CACHE_KO[cacheState(a)] || esc(cacheState(a))}</span>${cacheExp(a) && cacheState(a) !== "cold" ? ` <span class="cd" data-exp="${cacheExp(a)}" data-basis="${esc(cacheBasis(a))}"></span>` : ""}</dd>` : ""}
          ${a.tool ? `<dt>도구</dt><dd>${esc(a.tool)}</dd>` : ""}
          ${pr ? `<dt>진행</dt><dd>${pr.done}/${pr.total}${pr.current ? " · " + esc(pr.current) : ""} <span class="muted small">(${esc(pr.source)})</span></dd>` : ""}
          <dt>대화 출처</dt><dd>${tx ? (tx.unavailable ? `<span class="muted">불러올 수 없음 (${esc(tx.unavailable)})</span>` : `${esc(tx.source || "?")} · ${tx.mappingConfidence === "exact" ? "정확" : tx.mappingConfidence === "heuristic" ? `<span style="color:var(--info-warn)">추정</span>` : esc(tx.mappingConfidence || "?")}`) : "…"}</dd>
          <dt>경로</dt><dd><code class="path">${esc(a.worktreePath || "")}</code></dd></dl>${usageHtml(a)}`;
    }
    if (html !== ov.lastInfoHtml) { $("#info-pane").innerHTML = html; ov.lastInfoHtml = html; }
    if (a && !isMobile()) uhMount(a, $("#info-pane"));
    // Phone: the same usage section folds above the chat (closed by default; the open state is kept).
    const um = $("#usage-m"), uh = a && isMobile() ? usageHtml(a, true) : "";
    um.hidden = !uh;
    if (uh && uh !== ov.lastUsageM) { const open = um.open; um.innerHTML = uh; um.open = open; ov.lastUsageM = uh; }
    if (uh && a) uhMount(a, um);
    // agy conversations decoded from the db keep the raw terminal screen as a second view.
    // every agent with a terminal: its screen as a second view, with keys to send (login screens, pickers…)
    const hasTerm = !!a?.handle && !FIXTURE;
    $("#view-seg").hidden = !hasTerm;
    if (!hasTerm && ov.view === "terminal") setView("chat");
  }

  const tsTitle = (ts) => (ts ? new Date(ts).toLocaleString("ko-KR") : "");
  const toolRow = (m) => `<div class="trow" title="${esc(tsTitle(m.ts))}">▸ <b>${esc(m.tool?.name || "tool")}</b> ${esc(m.tool?.summary || m.text || "")}</div>`;

  // ---------- empty-chat opener ----------
  // One line per chat, picked at random the first time it is shown and then kept (no flicker on re-render).
  // Templates may use {name}, {harness}, {project}. Kept warm and plain: no sarcasm, no emoji.
  const WELCOME = {
    ko: [
      "{name}이(가) 커피를 내리며 첫 지시를 기다리는 중",
      "빈 화면은 가능성으로 가득합니다. {name}에게 첫 줄을 던져 보세요",
      "프롬프트 캐시도 아직 차갑습니다. 첫 메시지로 데워 주세요",
      "{name} 준비 완료. 무엇부터 시작할까요?",
      "{project}의 새 장이 열렸습니다. 첫 문장을 써 주세요",
      "{harness} 세션이 막 깨어났습니다. 오늘의 첫 부탁은?",
      "{name}이(가) 연필을 깎아 두었습니다",
      "아직 조용합니다. 한 줄이면 {name}이(가) 움직이기 시작합니다",
      "첫 메시지는 늘 가장 설레는 법이죠",
      "{name}의 작업대가 깨끗하게 치워져 있습니다",
      "생각해 둔 계획을 들려주세요. {name}이(가) 귀 기울이고 있어요",
      "새 세션, 새 출발. {project}에서 무엇을 해 볼까요?",
      "{name}이(가) 시동을 걸고 기다리는 중입니다",
    ],
    en: [
      "Tabula rasa. {name} is all ears.",
      "No messages yet. {name} has read zero lines of your mind.",
      "The prompt cache is still cold. Warm it up with a first message.",
      "{name} is ready. Where should we start?",
      "A fresh page for {project}. Write the first line.",
      "A new {harness} session just woke up. What's the first task?",
      "{name} has sharpened its pencils.",
      "All quiet for now. One line and {name} gets moving.",
      "First messages are the exciting ones.",
      "{name}'s workbench is clean and ready.",
      "Tell {name} the plan. It's listening.",
      "New session, fresh start. What shall we build in {project}?",
      "{name} has the engine running and is waiting for directions.",
    ],
  };
  const welcomePick = new Map();
  function welcomeHtml(tx) {
    const lang = window.ADASH_LANG === "en" ? "en" : "ko", list = WELCOME[lang];
    const key = `${lang}:${ov.name}`;
    if (!welcomePick.has(key)) welcomePick.set(key, Math.floor(Math.random() * list.length));
    const a = agentBy(ov.name);
    const vars = { name: ov.name || "", harness: (a?.agentType === "antigravity" ? "agy" : a?.agentType) || "agent", project: a?.project || "" };
    const line = list[welcomePick.get(key)].replace(/\{(name|harness|project)\}/g, (_, k) => vars[k]);
    const tip = lang === "en" ? "Type / for skills, @ for files, or paste or drop any file." : "/ 로 스킬, @ 로 파일, 파일 붙여넣기·끌어다 놓기 가능";
    const pending = tx.source === "pending";
    const wait = pending ? (lang === "en" ? "The conversation appears here once you send the first message." : (tx.hint || "첫 메시지를 보내면 대화가 여기에 표시됩니다")) : "";
    return `<div class="welcome" data-no-i18n><svg viewBox="0 0 24 24" width="28" height="28" aria-hidden="true"><path d="M4 5.5h16v10H9l-4 3.5v-3.5H4z" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/></svg><p class="w-line">${esc(line)}</p><p class="w-tip">${esc(tip)}</p>${wait ? `<p class="w-wait">${esc(wait)}${tx.terminalAvailable !== false ? ` <button type="button" data-v="terminal" data-act="view">${lang === "en" ? "View terminal" : "터미널 보기"}</button>` : ""}</p>` : ""}</div>`;
  }
  function renderChat(initial = false) {
    if (!ov.open || !ov.name) return;
    const log = $("#chat-log");
    const tx = ov.tx;
    const msgs = tx?.messages || [];
    // Drop pending sends that now appear in the transcript.
    const users = msgs.filter((m) => m.role === "user").map((m) => splitAttachments(m.text, m.attachments));
    const norm = (s) => String(s || "").replace(/\s+/g, " ").trim();
    // Pending ↔ transcript match (v46): the server may alter the text (U+200B before a leading "#"/"!", newline
    // normalization, attachment lines) and pasted rendered math is full of line breaks and zero-width spaces, so
    // compare with all whitespace and zero-width characters removed, NFC, and allow containment either way.
    const nz = (s) => String(s || "").normalize("NFC").replace(/[\s\u200b-\u200d\u2060\ufeff]+/g, "");
    const tsMs = (t) => (typeof t === "number" ? (t < 1e12 ? t * 1000 : t) : Date.parse(t || "") || 0);
    const userTs = msgs.filter((m) => m.role === "user").map((m) => tsMs(m.ts));
    const st0 = agentBy(ov.name);
    ov.pending = ov.pending.filter((p) => {
      // A "/btw …" send shows up as a btw question record (kind "btw", role user) with the same body, after the
      // btw questions that already existed when it was sent.
      let seen;
      if (p.btwText != null) seen = msgs.filter((m) => (m.kind === "btw" || m.btw) && m.role === "user").slice(p.seenBtw || 0).some((m) => norm(m.text) === norm(p.btwText));
      else {
        // user messages that arrived after the send: past the count seen at send time, or stamped at/after it
        const fresh = users.filter((u, k) => k >= p.seenUsers || userTs[k] >= p.at - 2000);
        const want = nz(p.text);
        seen = fresh.some((u) => {
          const got = nz(u.body);
          if (want) return !!got && (got === want || got.includes(want) || want.includes(got));
          return p.images.every((x) => u.paths.includes(x)) && (p.files || []).every((x) => u.files.includes(x));
        });
        // Text still differs: any new user message, or the agent starting to work after the send, means it arrived.
        if (!seen && (fresh.length || (st0?.state === "working" && (st0.stateSince || 0) >= p.at - 2000))) seen = true;
      }
      if (seen) { p.urls.forEach((u) => URL.revokeObjectURL(u)); return false; }
      // Never "sending" forever: after 30 s it turns into "not confirmed · send again".
      if (!p.stale && Date.now() - p.at > 30000) p.stale = true;
      return true;
    });
    if (ov.pending.some((p) => !p.stale)) { clearTimeout(ov.pendTimer); ov.pendTimer = setTimeout(() => ov.open && renderChat(), 31000); }
    const blocks = []; // [key, html]: each html is exactly one element
    if (tx?.unavailable) blocks.push(["unavail", `<div class="m system">대화 기록을 불러올 수 없어 마지막 메시지만 보여 줍니다.</div>`]);
    for (let i = 0; i < msgs.length; i++) {
      const m = msgs[i];
      if (m.role === "tool") {
        const group = [m];
        while (i + 1 < msgs.length && msgs[i + 1].role === "tool") group.push(msgs[++i]);
        if (group.length === 1) blocks.push([m.id, toolRow(m)]);
        else {
          const names = [...new Set(group.map((g) => g.tool?.name || "tool"))].join(", ");
          blocks.push([`g:${group[0].id}`, `<details class="tg" data-gid="${esc(group[0].id)}"${ov.openGroups.has(group[0].id) ? " open" : ""}><summary>도구 ${group.length}개 · ${esc(names)}</summary>${group.map(toolRow).join("")}</details>`]);
        }
      } else if (m.kind === "btw" || m.btw) { const open = !!agentBy(ov.name)?.btw?.open; blocks.push([`${m.id}:${m.status || ""}:${open}`, btwBubble(m, msgs, open)]); }
      else if (m.role === "user") blocks.push([m.id, userBubble(m)]);
      else if (m.role === "assistant") blocks.push([m.id, `<div class="m assistant md" title="${esc(tsTitle(m.ts))}">${msgCopyBtn(m.text)}${md(m.text, ov.name, m.id ?? m.ts)}</div>`]);
      else if (m.role === "error") blocks.push([m.id, errorBubble(m)]);
      else if (m.role === "shell") blocks.push([m.id, shellBubble(m)]);
      else if (m.role === "media") blocks.push([m.id, mediaRow(m)]);
      // open state survives re-renders (same as tool groups); no text: no block
      else if (m.role === "thinking") { if (m.text?.trim()) blocks.push([`${m.id}:${ov.openGroups.has(m.id)}`, `<details class="m thinking" data-gid="${esc(m.id)}"${ov.openGroups.has(m.id) ? " open" : ""} title="${esc(tsTitle(m.ts))}"><summary>${t_("생각")}</summary><div class="md">${md(m.text, ov.name, m.id)}</div></details>`]); }
      else blocks.push([m.id, `<div class="m system${tx?.source === "terminal" ? " term" : ""}" title="${esc(tsTitle(m.ts))}">${esc(m.text)}</div>`]);
    }
    ov.pending.forEach((p, i) => blocks.push([`p:${i}:${p.stale ? 1 : 0}`, `<div class="m user pending md${p.stale ? " stale" : ""}">${p.text.trim() ? md(p.text, ov.name) : ""}${thumbs(p.urls.map((u, k) => [u, p.images[k]]))}${fileChips(p.files || [], p.sizes, false)}${p.stale ? `<div class="pend-st">${L2("전송 확인 안 됨", "Delivery not confirmed")} <button type="button" data-act="pending-resend" data-i="${i}">${L2("다시 보내기", "Send again")}</button> <button type="button" data-act="pending-drop" data-i="${i}">${L2("지우기", "Dismiss")}</button></div>` : ""}</div>`]));
    // Empty conversation (no messages yet, or a new session whose session file does not exist yet, source
    // "pending"): a friendly opener instead of a bare "메시지 없음". The pending launch prompt, if any, follows it;
    // when the session file maps, the next poll replaces everything.
    const talk = msgs.some((m) => m.role === "assistant" || (m.role === "user" && m.status !== "pending"));
    if (tx && !tx.unavailable && tx.source !== "terminal" && !talk && !ov.pending.length) blocks.unshift(["welcome", welcomeHtml(tx)]);
    if (!blocks.length) blocks.push(["empty", `<p class="muted">${tx ? "메시지 없음" : "불러오는 중…"}</p>`]);
    const nearBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 80;
    const hadContent = !!ov.lastLogHtml;
    // Keyed diff: reuse nodes whose key and html are unchanged, so only new messages are parsed into DOM.
    let changed = false;
    if (log._keyed !== true) { log.innerHTML = ""; log._keyed = true; }
    const tpl = document.createElement("template");
    blocks.forEach(([key, html], i) => {
      const cur = log.children[i];
      if (cur && cur._k === key && cur._h === html) return;
      tpl.innerHTML = html;
      const node = tpl.content.firstElementChild;
      node._k = key; node._h = html;
      if (cur) log.replaceChild(node, cur); else log.appendChild(node);
      changed = true;
    });
    while (log.children.length > blocks.length) { log.lastElementChild.remove(); changed = true; }
    if (!changed) return;
    ov.lastLogHtml = blocks.map((b) => b[0]).join("|");
    linkifyPaths(log, ov.name);
    linkifyPlainPaths(log, ov.name);
    if (cmdCache.has(ov.name)) highlightSkills(log, ov.name);
    else { const n = ov.name; loadCommands(n).then(() => { if (ov.name === n) highlightSkills($("#chat-log"), n); }); }
    enhanceRich(log);
    if (initial || !hadContent || nearBottom) { log.scrollTop = log.scrollHeight; $("#new-pill").hidden = true; }
    else $("#new-pill").hidden = false;
  }

  function renderWorkLine() {
    const el = $("#work-line");
    const a = ov.open && ov.name && agentBy(ov.name);
    if (!a || a.state !== "working") { el.hidden = true; return; }
    el.hidden = false;
    const tool = ov.tx?.tool || a.tool;
    const elapsed = (FIXTURE ? state.generatedAt + (Date.now() - loadedAt) : Date.now()) - (a.stateSince || Date.now());
    const html = `<span class="dot"></span>작업 중${tool ? ` · ${esc(tool)}` : ""} · ${fmtClock(elapsed)}<span class="spacer"></span><button data-act="interrupt" data-name="${esc(a.name)}">중단</button>`;
    if (el._h !== html) { el.innerHTML = html; el._h = html; }
  }

  function renderHeld() {
    const el = $("#held-bar");
    if (!ov.held) { el.hidden = true; return; }
    el.hidden = false;
    const msg = ov.held === "working" ? "에이전트가 작업 중입니다" : "입력창에 초안이 있을 수 있습니다";
    const html = `⚠ ${msg} — 아직 보내지 않았습니다.<span class="spacer"></span><button class="primary" data-act="send-force">그래도 전송</button><button data-act="held-cancel">취소</button>`;
    if (el._h !== html) { el.innerHTML = html; el._h = html; }
  }

  // ---------- attachments (composer) ----------
  // Any file: images get a thumbnail and go out as images[], everything else as files[]. Clipboard text that is a
  // file:// URI list (copied in a file manager) becomes local-path chips without uploading (checked with /stat).
  const IMG_TYPES = ["image/png", "image/jpeg", "image/webp", "image/gif"];
  const FILE_MAX = 100 * 1024 * 1024;
  // Per agent: attsBy.get(name) = [...]; ov.atts is the open chat's list. Upload callbacks write to the list they
  // started in, so a chip never moves to another agent's composer.
  const attsBy = new Map(), composerTextBy = new Map();
  const attsOf = (name) => { if (!attsBy.has(name)) attsBy.set(name, []); return attsBy.get(name); };
  Object.defineProperty(ov, "atts", { get: () => attsOf(ov.name || ""), set: (v) => attsBy.set(ov.name || "", v), configurable: true });
  // {id, name, size, kind: "image"|"file", url (image object URL), status: "uploading"|"checking"|"done"|"error", pct, path, local, err}
  let attSeq = 0;
  const fmtBytes = (n) => (n >= 1048576 ? `${(n / 1048576).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);
  const baseName = (p) => String(p).replace(/\/+$/, "").split("/").pop() || String(p);
  // Short type label for the file icon (flat box, no emoji).
  function extTag(name) {
    const e = (String(name).match(/\.([a-z0-9]+)$/i) || [])[1]?.toLowerCase() || "";
    if (e === "pdf") return "PDF";
    if (["zip", "gz", "tgz", "xz", "bz2", "zst", "7z", "rar", "tar"].includes(e)) return "ZIP";
    if (["txt", "md", "log", "csv", "tsv", "json", "jsonl", "yaml", "yml", "toml", "xml", "tex", "bib"].includes(e)) return e.length <= 4 ? e.toUpperCase() : "TXT";
    if (["png", "jpg", "jpeg", "gif", "webp", "svg"].includes(e)) return "IMG";
    return e && e.length <= 4 ? e.toUpperCase() : t_("파일");
  }
  const fileIcon = (name) => `<span class="att-ic" aria-hidden="true">${esc(extTag(name))}</span>`;
  function renderAtts() {
    const bar = $("#att-bar");
    bar.hidden = !ov.atts.length;
    bar.innerHTML = ov.atts.map((a) => {
      const st = a.status === "uploading" ? `${t_("올리는 중")} ${a.pct}%` : a.status === "checking" ? t_("확인 중") : a.status === "error" ? `<span class="att-err">${esc(t_("실패:"))} ${esc(a.err)}</span>` : a.local ? t_("로컬 파일") : t_("준비됨");
      const size = a.size != null ? `${fmtBytes(a.size)} · ` : "";
      return `<div class="att ${a.status}${a.kind === "file" ? " file" : ""}" title="${esc(a.err || a.path || a.name)}">
      ${a.kind === "image" && a.url ? `<img src="${esc(a.url)}" alt="">` : fileIcon(a.name)}
      <div class="att-meta"><span class="att-name">${esc(a.name)}</span><span class="muted">${size}${st}</span></div>
      ${a.status === "uploading" ? `<i class="att-prog" style="width:${a.pct}%"></i>` : ""}
      ${a.status === "error" && (a.file || a.local) ? `<button type="button" class="att-retry" data-att-retry="${a.id}">${esc(t_("다시 시도"))}</button>` : ""}
      <button type="button" class="att-x" data-att-rm="${a.id}" title="${esc(t_("빼기"))}" aria-label="${esc(t_("빼기"))}">✕</button></div>`;
    }).join("");
    const pending = ov.atts.some((a) => a.status === "uploading" || a.status === "checking");
    $("#send-btn").disabled = pending;
    $("#send-btn").title = pending ? t_("파일 올리는 중") : "";
  }
  function clearAtts() { ov.atts.forEach((a) => a.url && URL.revokeObjectURL(a.url)); ov.atts = []; renderAtts(); }
  function uploadXhr(file, onPct) {
    return new Promise((resolve, reject) => {
      const x = new XMLHttpRequest();
      x.open("POST", `/api/uploads?name=${enc(file.name || "file")}`);
      x.setRequestHeader("X-Adash", "1");
      // Header values must be ISO-8859-1: the name goes percent-encoded (Korean names would make the request throw).
      x.setRequestHeader("X-Adash-Filename", enc(file.name || "file"));
      x.setRequestHeader("Content-Type", file.type || "application/octet-stream");
      x.upload.onprogress = (e) => { if (e.lengthComputable) onPct(Math.round((100 * e.loaded) / e.total)); };
      x.onload = () => {
        let j = null; try { j = JSON.parse(x.responseText); } catch { /* not JSON */ }
        if (x.status >= 200 && x.status < 300 && j?.path) resolve(j); else reject(new Error(j?.error || `HTTP ${x.status}`));
      };
      x.onerror = () => reject(new Error(t_("네트워크 오류")));
      x.send(file);
    });
  }
  function addFiles(files) {
    if (!ov.open || !ov.name) return;
    for (const f of files) {
      if (f.size > FILE_MAX) { toast(L2(`너무 큼: ${f.name || "?"} ${fmtBytes(f.size)} (최대 100 MB)`, `Too large: ${f.name || "?"} ${fmtBytes(f.size)} (max 100 MB)`), 4000); continue; }
      const image = IMG_TYPES.includes(f.type);
      const ext = (f.name.match(/\.([a-z0-9]+)$/i) || [])[1] || (image ? f.type.split("/")[1] : "bin");
      const a = { id: ++attSeq, name: f.name || (image ? `${t_("붙여넣은 이미지")}.${ext}` : `${t_("붙여넣은 파일")}.${ext}`), size: f.size, kind: image ? "image" : "file", url: image ? URL.createObjectURL(f) : null, status: "uploading", pct: 0, file: f, ext };
      ov.atts.push(a);
      startUpload(a, ov.name);
    }
    renderAtts();
  }
  // Upload one chip; results land in the owning agent's list (and repaint only if that chat is open).
  function startUpload(a, owner) {
    const f = a.file;
    a.status = "uploading"; a.pct = 0; a.err = null;
    const paint = () => { if (ov.name === owner) renderAtts(); };
    const done = (fn) => (v) => { if (!attsOf(owner).includes(a)) return; fn(v); paint(); };
    const up = FIXTURE
      ? new Promise((res, rej) => { console.log("[fixture] would send POST /api/uploads X-Adash: 1 X-Adash-Filename:", enc(f.name || "file"), f.type || "application/octet-stream", f.size); let k = 0; const tick = setInterval(() => { k += 25; a.pct = Math.min(100, k); paint(); if (k >= 100) { clearInterval(tick); if (/fail/i.test(f.name)) return rej(new Error("file name required")); const path = `~/.local/share/adash/uploads/20261029/101500-${a.id.toString(16).padStart(10, "0")}.${a.ext}`; fixtureSizes.set(path, f.size); fixtureSizes.set(`name:${path}`, f.name); res({ path, name: f.name, bytes: f.size, mime: f.type }); } }, 150); })
      : uploadXhr(f, (p) => { a.pct = p; paint(); });
    up.then(done((r) => { a.status = "done"; a.path = r.path; }), done((e) => { a.status = "error"; a.err = e.message; }));
    paint();
  }
  // "file:///home/u/a%20b.pdf" lines (text/uri-list or the plain text a file manager copies) → local paths.
  function fileUriPaths(text) {
    const lines = String(text || "").split(/\r?\n/).map((l) => l.trim()).filter((l) => l && !l.startsWith("#"));
    if (lines[0] === "copy" || lines[0] === "cut") lines.shift();
    if (!lines.length || !lines.every((l) => /^file:\/\/[^/]*\//.test(l))) return null;
    try { return lines.map((l) => decodeURIComponent(new URL(l).pathname)); } catch { return null; }
  }
  async function addLocalPaths(paths) {
    if (!ov.open || !ov.name) return;
    const name = ov.name, list = attsOf(name);
    const added = paths.map((p) => ({ id: ++attSeq, name: baseName(p), size: null, kind: /\.(png|jpe?g|webp|gif)$/i.test(p) ? "image" : "file", url: null, status: "checking", pct: 0, path: p, local: true }));
    list.push(...added); renderAtts();
    let r = null;
    try { r = await api("POST", `/api/agents/${enc(name)}/stat`, { paths }, { quiet: true }); } catch (e) { r = { error: e.message }; }
    for (const a of added) {
      if (!list.includes(a)) continue;
      const hit = r?.results?.find((x) => x.path === a.path);
      if (!hit) { a.status = "error"; a.err = r?.error || t_("확인 실패"); }
      else if (!hit.exists) { a.status = "error"; a.err = t_("없는 파일"); }
      else if (hit.dir) { a.status = "error"; a.err = t_("폴더는 첨부할 수 없음"); }
      else { a.status = "done"; if (hit.size != null) a.size = hit.size; }
    }
    if (ov.name === name) renderAtts();
  }

  // Upload paths written into a sent message ("[첨부 이미지 k] <path> — …").
  // Server forms: "[첨부 이미지 N] <path> — …", "[첨부 파일 N] @<path>" (omp·claude), "<path> — 이 파일을 읽어 주세요" (codex·agy).
  const ATT_LINE = /^\[첨부 (이미지|파일) \d+\]\s+@?(.+?)(?:\s+—\s.*)?$/;
  const ATT_READ_LINE = /^@?((?:~|\/)\S.*?) — 이 파일을 읽어 주세요\.?$/;
  const IMG_EXT = /\.(png|jpe?g|webp|gif|svg)$/i;
  function splitAttachments(text, given) {
    const paths = [], files = [], sizes = new Map();
    for (const x of given || []) { if (!x?.path) continue; (x.kind === "image" || (x.kind !== "file" && IMG_EXT.test(x.path)) ? paths : files).push(x.path); if (x.size ?? x.bytes) sizes.set(x.path, x.size ?? x.bytes); if (x.name) sizes.set(`name:${x.path}`, x.name); }
    const body = String(text || "").split("\n").filter((l) => { const m = l.match(ATT_LINE); if (m) { (m[1] === "이미지" ? paths : files).push(m[2]); return false; } const r = l.match(ATT_READ_LINE); if (r) { (IMG_EXT.test(r[1]) ? paths : files).push(r[1]); return false; } return true; }).join("\n").replace(/\s+$/, "");
    return { body, paths: [...new Set(paths)], files: [...new Set(files)], sizes };
  }
  // Non-image attachments in a bubble: icon + name (+ size); a click opens it (PDF in the viewer).
  const fileChips = (files, sizes, link) => (files.length ? `<div class="fatts">${files.map((p) => { const inner = `${fileIcon(p)}<span class="fc-name">${esc(sizes?.get(`name:${p}`) || baseName(p))}</span>${sizes?.get(p) ? `<span class="fc-size">${fmtBytes(sizes.get(p))}</span>` : ""}`; return link ? `<a class="fchip" href="${esc(fileUrl(ov.name, p, Date.now()))}" data-local="${esc(p)}" data-agent="${esc(ov.name)}" title="${esc(p)}">${inner}</a>` : `<span class="fchip" title="${esc(p)}">${inner}</span>`; }).join("")}</div>` : "");
  const thumbs = (srcs) => (srcs.length ? `<div class="atts">${srcs.map(([src, p]) => `<img class="md-img att-img" src="${esc(src)}" data-path="${esc(p)}" alt="${esc(p)}" loading="lazy">`).join("")}</div>` : "");
  // Skill invocation (m.skill) and ask answers (kind "ask-answer" + answers[]) get a structured bubble; the raw
  // text stays in the tooltip/search. Same summary is used by 최근 내 활동.
  const askAnswersOf = (m) => (m && (m.kind === "ask-answer" || m.askAnswer) ? m.answers || m.askAnswer || null : null);
  function skillTag(skill, agent) {
    if (!skill?.name) return "";
    const it = cmdCache.get(agent)?.items?.find((x) => x.kind === "skill" && x.name === skill.name);
    const tip = `스킬 · ${skill.name}${skill.path ? `\n${skill.path}` : ""}${it?.description ? `\n${it.description}` : ""}${it ? "\n누르면 입력창에 넣기" : ""}`;
    return `<span class="skchip${it ? "" : " static"}"${it ? ` data-sk-insert="${esc(it.insert)}"${it.midInsert ? ` data-sk-mid="${esc(it.midInsert)}"` : ""}` : ""} title="${esc(tip)}">${ICONS.skill}<span>${esc(skill.name)}</span></span>`;
  }
  function answerSummaryHtml(answers) {
    return `<div class="aa">${answers.map((x) => {
      const q = String(x.header || x.question || "").split("\n")[0];
      const picked = (x.picked || []).map((p) => `<span class="aa-pick">${esc(p)}</span>`).join("");
      const own = x.text ? `<span class="aa-own" title="직접 입력">${esc(x.text)}</span>` : "";
      return `<div class="aa-r"><span class="aa-q" title="${esc(x.question || "")}">${esc(q.length > 80 ? `${q.slice(0, 79)}…` : q)}</span><span class="aa-to">→</span><span class="aa-a">${picked}${own || (picked ? "" : '<span class="muted">(답 없음)</span>')}</span></div>`;
    }).join("")}</div>`;
  }
  // ---------- open btw (v23): POST /api/agents/{name}/btw {text} · POST …/btw/close ----------
  ov.btwUnsupported = new Set();
  async function btwCall(name, what, text) {
    if (!name) return false;
    try {
      const r = await api("POST", `/api/agents/${enc(name)}/btw${what === "close" ? "/close" : ""}`, what === "close" ? {} : { text }, { quiet: true });
      if (what === "close") toast("btw 를 닫았습니다", 2000);
      else if (r?.held) toast(`btw 후속 질문 보류: ${HELD_KO[r.held] || r.held}`);
      else toast("btw 후속 질문 보냄", 2000);
      const s = agentBy(name);
      if (what === "close" && s) s.btw = { ...(s.btw || {}), open: false };
      if (!FIXTURE) refresh(); else render();
      if (ov.name === name) renderChat();
      return !r?.held;
    } catch (e) {
      if ([404, 405, 501].includes(e.status)) { ov.btwUnsupported.add(name); renderChat(); renderBtwNote(); toast("이 서버는 btw 후속 질문·닫기를 지원하지 않습니다"); }
      else toast(`btw ${what === "close" ? "닫기" : "후속 질문"} 실패: ${e.message}`);
      return false;
    }
  }
  // Above the composer while a btw is open: normal messages wait until it is closed.
  function renderBtwNote() {
    const el = $("#btw-note"), a = ov.open && ov.name ? agentBy(ov.name) : null, open = !!a?.btw?.open;
    el.hidden = !open;
    if (!open) return;
    H("#btw-note").innerHTML = `<span class="btw-tag">btw</span><span>btw 열림 · 일반 메시지는 닫은 뒤 보낼 수 있음</span><span class="spacer"></span>${ov.btwUnsupported.has(ov.name) ? "" : `<button type="button" data-act="btw-close">btw 닫기</button>`}`;
  }
  // omp "btw" side question (kind "btw"): kept apart from the main thread — faint dashed border + "btw" label;
  // a spinner while the question waits for its answer (status "pending") or the answer is still streaming.
  function btwBubble(m, msgs, open = false) {
    const answered = m.role === "user" && msgs.some((x) => x !== m && x.role === "assistant" && (x.kind === "btw" || x.btw) && x.btwId && x.btwId === m.btwId);
    const busy = (m.role === "user" && m.status === "pending" && !answered) || (m.role === "assistant" && m.status === "streaming");
    const label = m.role === "user" ? "btw 질문" : "btw 답";
    // error is {message, at, status, provider, model} (same shape as role:error); a plain string is accepted too.
    const errText = typeof m.error === "string" ? m.error : m.error?.message;
    const state = busy ? `<span class="btw-wait"><i class="spin"></i>${m.role === "user" ? "답 기다리는 중" : "작성 중"}</span>` : m.status === "error" ? `<span class="btw-err" title="${esc(m.error?.at ? new Date(m.error.at).toLocaleString("ko-KR") : "")}">${esc(errText || "오류")}</span>` : "";
    const body = m.text?.trim() ? md(m.text, ov.name, `${m.id ?? m.ts}:${m.status || ""}`) : "";
    // While the btw is open, the latest btw answer carries a follow-up row: [후속 질문…] [보내기] [btw 닫기].
    const lastAns = [...msgs].reverse().find((x) => (x.kind === "btw" || x.btw) && x.role === "assistant");
    const follow = open && m === lastAns && !ov.btwUnsupported?.has(ov.name)
      ? `<form class="btw-follow" data-btw-follow><input name="q" placeholder="후속 질문…" autocomplete="off" enterkeyhint="send"><button type="submit" class="primary">보내기</button><button type="button" data-act="btw-close">btw 닫기</button></form>` : "";
    return `<div class="m ${m.role === "user" ? "user" : "assistant"} btw" title="${esc(tsTitle(m.ts))}"><div class="btw-head"><span class="btw-tag" title="본 대화와 별개인 곁가지 질문 (omp btw)">btw</span><span class="muted small">${label}</span>${state}</div>${body ? `<div class="md">${body}</div>` : ""}${follow}</div>`;
  }
  // A shell command I ran through the harness (transcript role "shell"): command in monospace, output folded,
  // non-zero exit marked red.
  function shellBubble(m) {
    const sh = m.shell || m, cmd = sh.command ?? m.text ?? "", out = String(sh.output ?? ""), code = sh.exitCode;
    const bad = code != null && code !== 0, lines = out ? out.replace(/\n$/, "").split("\n").length : 0;
    return `<div class="m user shell${bad ? " bad" : ""}" title="${esc(tsTitle(m.ts))}"><div class="sh-head"><span class="muted small">내가 실행한 셸 명령</span>${code != null ? `<span class="sh-exit">${bad ? `exit ${esc(code)}` : "exit 0"}</span>` : ""}</div>
      <pre class="sh-cmd"><span class="sh-ps">$</span> ${esc(cmd)}</pre>${out ? `<details class="sh-out"${bad ? " open" : ""}><summary>출력 ${lines}줄</summary><pre>${esc(out)}</pre></details>` : ""}</div>`;
  }

  // ---------- harness input prefixes (v20): e.g. omp runs "!…" as a shell command ----------
  // Prefixes come with the agent's catalog (`prefixes:[{prefix, kind, warning}]`); without that field omp gets "!".
  // The warning offers 텍스트로 보내기 (default → send with asText) or 셸 명령으로 실행 (send as typed).
  ov.prefixMode = "text"; ov.prefixHit = null;
  function prefixesOf(name) {
    const c = cmdCache.get(name);
    if (c?.prefixes) return c.prefixes;
    return String(agentBy(name)?.agentType || "").toLowerCase() === "omp" ? [{ prefix: "!", kind: "shell", warning: "! at the start runs a shell command in omp" }] : [];
  }
  function updatePrefixWarn() {
    const el = $("#prefix-warn"), v = $("#composer-text").value;
    const hit = ov.open && ov.name ? prefixesOf(ov.name).filter((p) => p.prefix && v.startsWith(p.prefix)).sort((a, b) => b.prefix.length - a.prefix.length)[0] : null;
    if (!hit) { el.hidden = true; ov.prefixHit = null; ov.prefixMode = "text"; return; }
    // A new prefix (or a pasted image Markdown "![…](…)") starts on 텍스트로 보내기.
    if (ov.prefixHit?.prefix !== hit.prefix) ov.prefixMode = "text";
    if (/^!\[[^\]]*\]\(/.test(v)) ov.prefixMode = "text";
    ov.prefixHit = hit;
    const kindKo = hit.kind === "shell" ? "셸 명령" : hit.kind === "memory" ? "메모리 추가" : "특수 명령";
    H("#prefix-warn").innerHTML = `<span class="pw-msg">${ICONS.err}${esc(hit.warning || `${hit.prefix} 로 시작하면 ${kindKo}으로 처리됩니다`)}</span><span class="seg pw-seg"><button type="button" data-prefix-mode="text" class="${ov.prefixMode === "text" ? "on" : ""}">텍스트로 보내기</button><button type="button" data-prefix-mode="raw" class="${ov.prefixMode === "raw" ? "on" : ""}">${esc(kindKo)}으로 실행</button></span>`;
    el.hidden = false;
  }
  $("#prefix-warn").addEventListener("mousedown", (e) => {
    const b = e.target.closest("[data-prefix-mode]");
    if (!b) return;
    e.preventDefault();
    ov.prefixMode = b.dataset.prefixMode;
    updatePrefixWarn();
  });
  // Provider/API error that stopped the agent (transcript role "error"): status · message · provider/model · time,
  // plus [이어서 진행], which only fills the composer; sending stays with the user.
  function errorBubble(m) {
    const e = m.error || m.lastError || {};
    const status = e.status ?? m.status, msg = e.message || m.text || "", prov = [e.provider || m.provider, e.model || m.model].filter(Boolean).join(" / ");
    const at = e.at || m.ts;
    return `<div class="m error-msg"><div class="em-head">${ICONS.err}<b>${esc(errLabel({ status }))}</b>${prov ? `<span class="muted small">${esc(prov)}</span>` : ""}<span class="spacer"></span><span class="muted small">${at ? esc(new Date(at).toLocaleString("ko-KR")) : ""}</span></div>
      <div class="em-body">${esc(msg)}</div>
      <div class="em-act"><span class="muted small">제공자 API 오류로 멈췄습니다.</span><span class="spacer"></span><button class="primary" data-retry="${esc(ov.name)}" title="${esc(retryTip(agentBy(ov.name)))}">${esc(retryLabel(agentBy(ov.name)))}</button><button data-act="resume" title="입력창에 '계속 진행해'를 넣습니다. 보내기는 직접 누르세요.">이어서 진행</button></div></div>`;
  }
  // Images a tool returned (the agent showed them): thumbnails in the chat, outside the folded tool
  // rows; a click opens the viewer. The bytes load lazily from the session file.
  function mediaRow(m) {
    const src = (x) => agentApiPath(`/api/agents/${enc(ov.name)}/attachment?msg=${enc(m.id)}&i=${x.i}`);
    const imgs = (m.images || []).map((x) => [src(x), `${t_("도구가 보여 준 이미지")} ${x.i + 1}`]);
    return `<div class="m media md" title="${esc(tsTitle(m.ts))}">${thumbs(imgs)}</div>`;
  }
  function userBubble(m) {
    const { body, paths, files, sizes } = splitAttachments(m.text, m.attachments);
    const ans = askAnswersOf(m);
    if (ans?.length) return `<div class="m user ask-ans" title="${esc(`${tsTitle(m.ts)}\n\n${m.text || ""}`)}"><div class="aa-head muted small">${t_("질문에 답함")}${esc(answeredVia(m))}</div>${answerSummaryHtml(ans)}</div>`;
    return `<div class="m user md${m.status === "pending" ? " pending" : ""}" title="${esc(tsTitle(m.ts))}">${msgCopyBtn(body)}${m.skill ? `<div class="ub-skill">${skillTag(m.skill, ov.name)}</div>` : ""}${body.trim() ? md(body, ov.name, m.id ?? m.ts) : ""}${thumbs(paths.map((p) => [FIXTURE && !p.startsWith("/") ? p : fileUrl(ov.name, p, m.id ?? m.ts), p]))}${fileChips(files, sizes, true)}</div>`;
  }
  // Copy a whole message as its source Markdown (math stays $…$, not KaTeX's MathML + HTML text).
  function msgCopyBtn(text) { return String(text || "").trim() ? `<button type="button" class="msg-copy" data-copy="${esc(text)}" title="${esc(L2("원문 마크다운 복사", "Copy as Markdown"))}">${L2("복사", "Copy")}</button>` : ""; }
  // Selection copy inside rendered Markdown: text/plain gets Markdown (math as its $…$ source, code with backticks,
  // list / heading / table marks kept) instead of KaTeX's MathML + HTML text split one glyph per line.
  function selToMd(node) {
    if (node.nodeType === 3) return node.nodeValue;
    if (node.nodeType !== 1 && node.nodeType !== 11) return "";
    // List and table containers: element children only (the whitespace text between <li>/<tr> is layout).
    const el = node, tag = el.nodeName.toLowerCase(), kids = () => [...(/^(ul|ol|table|thead|tbody|tr)$/.test(tag) ? el.children : el.childNodes)].map(selToMd).join("");
    if (el.classList?.contains("mx")) return el.dataset.md || "";
    if (el.classList?.contains("katex-mathml") || el.classList?.contains("msg-copy") || el.classList?.contains("copy")) return "";
    // texmath wraps display math in <section><eqn>; code fences sit in <div class="code">: both are blocks.
    if (tag === "section" || tag === "div") return `\n${kids().trim()}\n\n`;
    switch (tag) {
      case "br": return "\n";
      case "code": return el.closest?.("pre") || el.parentNode?.nodeName === "PRE" ? el.textContent : `\`${el.textContent}\``;
      case "pre": return `\n\`\`\`\n${el.textContent.replace(/\n$/, "")}\n\`\`\`\n\n`;
      case "p": return `${kids()}\n\n`;
      case "h1": case "h2": case "h3": case "h4": case "h5": case "h6": return `${"#".repeat(+tag[1])} ${kids()}\n\n`;
      case "strong": case "b": return `**${kids()}**`;
      case "em": case "i": return `*${kids()}*`;
      case "a": return el.getAttribute("href") ? `[${kids()}](${el.getAttribute("href")})` : kids();
      case "ul": case "ol": return `${kids()}\n`;
      case "li": {
        const p = el.parentNode, ol = p?.nodeName === "OL";
        const n = ol ? (+p.getAttribute("start") || 1) + [...p.children].indexOf(el) : 0;
        return `${ol ? `${n}. ` : "- "}${kids().replace(/\n+$/, "").replace(/\n/g, "\n   ")}\n`;
      }
      case "blockquote": return `${kids().trim().split("\n").map((l) => `> ${l}`).join("\n")}\n\n`;
      case "tr": {
        const row = `| ${[...el.children].map((c) => selToMd(c).replace(/\n+/g, " ").trim()).join(" | ")} |\n`;
        return el.parentNode?.nodeName === "THEAD" ? row + `|${[...el.children].map(() => "---").join("|")}|\n` : row;
      }
      case "table": return `${kids()}\n`;
      default: return kids();
    }
  }
  document.addEventListener("copy", (e) => {
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount) return;
    const r = sel.getRangeAt(0), anc = r.commonAncestorContainer.nodeType === 1 ? r.commonAncestorContainer : r.commonAncestorContainer.parentElement;
    if (!anc?.closest(".md, #decision-card, #viewer")) return;
    // Selection entirely inside one formula: its source.
    const inMath = anc.closest(".mx");
    const frag = r.cloneContents();
    const text = inMath ? inMath.dataset.md : selToMd(frag).replace(/(\|\n)\n+(?=\|)/g, "$1").replace(/\n{3,}/g, "\n\n").trim();
    const box = document.createElement("div"); box.appendChild(frag);
    e.clipboardData.setData("text/plain", text);
    e.clipboardData.setData("text/html", inMath ? inMath.outerHTML : box.innerHTML);
    e.preventDefault();
  });

  async function sendComposer(force, netConfirmed = false) {
    const ta = $("#composer-text");
    const text = ta.value.replace(/\s+$/, "");
    const name = ov.name;
    if (!name) return;
    if (answerJobs.get(name)?.state === "running") { toast("답을 넣는 중입니다. 끝난 뒤 보내세요"); return; }
    // An open btw takes the agent's input: only "/btw …" follow-ups go through until it is closed.
    if (agentBy(name)?.btw?.open && !/^\/btw\b/.test(text.trim())) { toast("btw 가 열려 있습니다. 'btw 닫기' 후 보내세요 (후속 질문은 btw 답 아래 입력줄)"); return; }
    if (ov.atts.some((a) => a.status === "uploading" || a.status === "checking")) { toast(t_("파일을 올리는 중입니다")); return; }
    const ready = ov.atts.filter((a) => a.status === "done");
    if (!text.trim() && !ready.length) return;
    const imgAtts = ready.filter((a) => a.kind === "image"), fileAtts = ready.filter((a) => a.kind !== "image");
    const images = imgAtts.map((a) => a.path), files = fileAtts.map((a) => a.path);
    $("#send-result").textContent = "보내는 중…";
    let r;
    // Special harness prefix (e.g. omp "!" = shell) with "텍스트로 보내기" chosen: ask the server to send it as plain text.
    // Provider unreachable (v26): confirm before a user send; the server still delivers it, with warning "network_down".
    const np = netOf(agentBy(name));
    if (np && !netConfirmed) {
      $("#send-result").textContent = "";
      dialog(`<h3>네트워크 확인</h3><p>제공자에 접속할 수 없습니다 (${esc(provLabel(np.provider))}: ${esc(NET_KO[np.state] || np.state)}). 그래도 보낼까요?</p><p class="muted small">에이전트 입력창에는 들어가지만 응답이 오류로 끝날 수 있습니다. VPN·네트워크를 먼저 확인하세요.</p>`, "그래도 보내기", () => sendComposer(force, true));
      return;
    }
    const asText = !!(ov.prefixHit && ov.prefixMode === "text");
    const sendBody = { text, force: !!force, ...(images.length ? { images } : {}), ...(files.length ? { files } : {}), ...(asText ? { asText: true } : {}) };
    try { r = await api("POST", `/api/agents/${enc(name)}/send`, sendBody); }
    catch (e) { $("#send-result").textContent = `실패: ${e.message}`; return; }
    if (name !== ov.name) return;
    if (r?.warning === "network_down") toast(`${name} · 보냈지만 제공자에 접속할 수 없는 상태입니다`, 4000);
    if (r && r.delivered) {
      // A delivered message answers the open decisions (server rule); clear them until the next state confirms.
      { const s0 = agentBy(name); if (s0 && decisionsOf(s0).length) { setDecFold(decKey(name, decisionsOf(s0)), true); s0.decisions = []; s0.decisionsAnswered = true; } }
      ov.held = null;
      const seenUsers = (ov.tx?.messages || []).filter((m) => m.role === "user").length;
      // "/btw …": the transcript records it as a btw question (kind "btw"); match on that text instead.
      const btwText = r.kind === "btw" || /^\s*\/btw\b/.test(text) ? String(r.btwText ?? text.replace(/^\s*\/btw\s*/, "")) : null;
      // Object URLs move to the pending bubble; they are revoked when the transcript shows the real message.
      const seenBtw = (ov.tx?.messages || []).filter((m) => (m.kind === "btw" || m.btw) && m.role === "user").length;
      if (ov.tx?.source !== "terminal") ov.pending.push({ text, images, urls: imgAtts.map((a) => a.url), files, sizes: new Map([...fileAtts.filter((a) => a.size != null).map((a) => [a.path, a.size]), ...fileAtts.map((a) => [`name:${a.path}`, a.name])]), seenUsers, seenBtw, btwText, at: Date.now() });
      else ready.forEach((a) => a.url && URL.revokeObjectURL(a.url));
      ta.value = ""; composerTextBy.delete(name);
      ov.atts = ov.atts.filter((a) => !ready.includes(a)); renderAtts();
      // Delivered: close the chat and return home (phones: the 내 차례 list). Held sends stay open (below).
      // A btw question keeps the chat open: its answer arrives right here.
      toast(`${name} · 전달됨 (${btwText != null ? "btw" : MODE_KO[r.mode] || r.mode || "?"}${r.file ? ` · ${r.file}` : ""}${images.length ? L2(` · 이미지 ${images.length}장`, ` · ${images.length} image${images.length > 1 ? "s" : ""}`) : ""}${files.length ? L2(` · 파일 ${files.length}개`, ` · ${files.length} file${files.length > 1 ? "s" : ""}`) : ""})`, 2500);
      if (btwText != null) { $("#send-result").textContent = ""; renderChat(); return; }
      $("#send-result").textContent = "";
      renderHeld();
      if (isMobile()) { mtab = "mine"; localStorage.setItem("adash.mtab", mtab); }
      closeChat();
      if (isMobile()) window.scrollTo(0, 0);
    } else if (r && r.held === "trust" && r.queued) {
      // kept by the server and sent once the folder is trusted
      ta.value = ""; ov.atts = [];
      $("#send-result").textContent = L2("대기 중(폴더 신뢰 필요) · 신뢰하면 자동으로 보냅니다", "Queued (folder trust needed) · sent once trusted");
      renderAuthCard();
    } else if (r && r.held) {
      ov.held = r.held;
      $("#send-result").textContent = "";
      renderHeld();
    } else $("#send-result").textContent = `전달되지 않음${r ? `: ${JSON.stringify(r)}` : ""}`;
  }

  function switchWaiting(dir) {
    const order = waitingOrder();
    if (!order.length) return;
    const i = order.indexOf(ov.name);
    const next = order[i < 0 ? (dir > 0 ? 0 : order.length - 1) : (i + dir + order.length) % order.length];
    openChat(next, "info");
  }

  function renderThreads() {
    const ts = state.threads || [];
    const runs = magiRuns().filter((r) => r.phase !== "cancelled");
    const runsHtml = runs.length ? `<div class="mg-sec-runs"><div class="muted small">magi 요청 ${runs.length}</div>${runs.map((r) => magiRunHtml(r, true)).join("")}</div>` : "";
    H("#threads").innerHTML = runsHtml + (ts.length ? `<table><thead><tr><th>상태</th><th>보낸→받는</th><th>요청 / 답변</th></tr></thead><tbody>` +
      ts.map((t) => `<tr><td><span class="st ${esc(t.status)}">${esc(STATUS_KO[t.status] || t.status)}</span><div class="muted small">홉 ${t.hops ?? "?"}</div>
        ${t.status === "relay_held" && t.heldReason ? `<div class="muted small">${esc(HELD_KO[t.heldReason] || t.heldReason)}</div>` : ""}
        ${t.status === "held_possible_draft" ? `<button class="primary" data-act="deliver" data-id="${esc(t.id)}">그래도 전송</button>` : t.status === "relay_held" ? `<button class="primary" data-act="relay-force" data-id="${esc(t.id)}" data-to="${esc(t.to)}">초안과 함께 전달</button>` : ""}</td>
        <td>${esc(t.from || "사람")} → ${esc(t.to)}<div class="muted small">${t.createdAt ? new Date(t.createdAt).toLocaleString("ko-KR") : ""}</div></td>
        <td>${esc(t.request)}${t.replyPreview ? `<div class="muted">↳ ${esc(t.replyPreview)}</div>` : ""}<div><code class="path">${esc(t.dir)}</code></div></td></tr>`).join("") +
      `</tbody></table>` : `<p class="muted">스레드 없음</p>`);
  }

  function renderTasks() {
    const pj = $("#ht-project"), cur = pj.value;
    H("#ht-project").innerHTML = `<option value="">(프로젝트 없음)</option>` + (state.projects || []).map((p) => `<option>${esc(p.name)}</option>`).join("");
    pj.value = cur;
    H("#humanTasks").innerHTML = (state.humanTasks || []).map((t) => `<li class="${t.done ? "done" : ""}">
      <label><input type="checkbox" data-act="task-done" data-id="${esc(t.id)}" ${t.done ? "checked" : ""}> <span>${esc(t.title)}</span></label>
      <span class="muted small">${esc(t.project || "")}${t.due ? " · 마감 " + esc(t.due) : ""}</span></li>`).join("") || `<li class="muted">없음</li>`;
  }

  // ---------- meta editor ----------
  async function loadMeta() {
    $("#meta-editor").innerHTML = `<p class="muted">불러오는 중…</p>`;
    try { meta = await api("GET", "/api/meta"); } catch { return; }
    meta.projects = meta.projects || {};
    for (const p of state?.projects || []) if (!meta.projects[p.name]) meta.projects[p.name] = { paths: p.paths || [], goal: "", impact: 3, leverage: 3, cost: 3, deadline: null, note: "" };
    const num = (n, k, v) => `<input type="number" min="1" max="5" data-p="${esc(n)}" data-k="${k}" value="${esc(v ?? 3)}">`;
    const hint = !model.scored && !localStorage.getItem("adash.scoreHintSeen") ? `<p class="meta-hint">모든 프로젝트가 기본 점수(impact·leverage·cost = 3)라서 보드의 ★ 점수를 숨겨 두었습니다. 한 프로젝트라도 점수를 바꾸면 ★가 다시 나타나고 '빠뜨린 것 먼저' 정렬에 반영됩니다.</p>` : "";
    if (hint) localStorage.setItem("adash.scoreHintSeen", "1");
    // One line under each field (v0.1.0). The ★ rule is fixed: (impact + 2×leverage) / cost.
    const help = (ko, en) => `<span></span><span class="mp-help muted small">${esc(L2(ko, en))}</span>`;
    $("#meta-editor").innerHTML = hint + `<p class="meta-rule muted small">${esc(L2("★ = (영향 + 2×지렛대) / 비용 · 모든 프로젝트가 기본값(3·3·3)이면 ★ 숨김 · '빠뜨린 것 먼저' 정렬에 반영", "★ = (impact + 2×leverage) / cost · hidden while every project is at the default (3·3·3) · used by the 'Missed first' sort"))}</p><div class="row" style="margin-bottom:8px"><button class="primary" id="meta-save">${esc(t_("프로젝트 설정 저장"))}</button></div>` +
      Object.entries(meta.projects).map(([n, p]) => `<div class="meta-proj"><h4>${esc(n)}</h4>
      <label>${esc(t_("목표"))}</label><input data-p="${esc(n)}" data-k="goal" value="${esc(p.goal)}">${help("이 프로젝트가 끝나면 무엇이 되는지 한 줄", "What done looks like, in one line")}
      <label>${esc(L2("영향", "impact"))}</label>${num(n, "impact", p.impact)}${help("1–5 · 끝났을 때 얼마나 중요한가", "1–5 · how much it matters once done")}
      <label>${esc(L2("지렛대", "leverage"))}</label>${num(n, "leverage", p.leverage)}${help("1–5 · 다른 일을 얼마나 풀어 주는가 (막힌 일, 재사용) · ★에서 두 배", "1–5 · how much other work it unblocks or enables · counts double in ★")}
      <label>${esc(L2("비용", "cost"))}</label>${num(n, "cost", p.cost)}${help("1–5 · 남은 노력과 시간 · 클수록 ★가 낮아짐", "1–5 · remaining effort and time · higher lowers ★")}
      <label>${esc(t_("마감"))}</label><input type="date" data-p="${esc(n)}" data-k="deadline" value="${esc(p.deadline || "")}">${help("선택 · 이 프로젝트의 마감일", "Optional · this project's due date")}
      <label>${esc(t_("경로"))}</label><textarea data-p="${esc(n)}" data-k="paths" rows="2">${esc((p.paths || []).join("\n"))}</textarea>${help("이 프로젝트로 묶을 작업 폴더 · 한 줄에 하나", "Working folders that belong to this project · one per line")}
      <label>${esc(t_("메모"))}</label><input data-p="${esc(n)}" data-k="note" value="${esc(p.note)}">${help("나만 보는 메모", "A private note")}</div>`).join("");
  }
  function saveMeta() {
    for (const el of $$("#meta-editor [data-p]")) {
      const p = meta.projects[el.dataset.p], k = el.dataset.k, v = el.value;
      if (k === "paths") p.paths = v.split("\n").map((s) => s.trim()).filter(Boolean);
      else if (["impact", "leverage", "cost"].includes(k)) p[k] = Math.min(5, Math.max(1, parseInt(v, 10) || 3));
      else if (k === "deadline") p[k] = v || null;
      else p[k] = v;
    }
    act("PUT", "/api/meta", meta, t_("프로젝트 설정 저장됨"));
  }

  // ---------- dialogs ----------
  function dialog(html, okLabel, onOk) {
    const d = $("#dlg"), f = $("#dlg-form");
    f.innerHTML = html + `<div class="row" style="margin-top:10px;justify-content:flex-end"><button value="cancel" formnovalidate>취소</button><button value="ok" class="primary">${esc(okLabel)}</button></div>`;
    d.returnValue = "";
    d.onclose = () => { if (d.returnValue === "ok") onOk(new FormData(f)); render(); };
    d.showModal();
    f.querySelector("textarea,input,select")?.focus();
  }
  // 터미널 열기: POST /focus → {opened:true, terminal} | {opened:false, client} (an attached tmux client already
  // shows it) | {opened:false, command, reason} (no terminal emulator found / launch failed): offer the attach command.
  async function openTerminal(name) {
    let r;
    try { r = await api("POST", `/api/agents/${enc(name)}/focus`, {}, { quiet: true }); }
    catch (e) { toast(`터미널 열기 실패: ${e.message}`, 5000); return; }
    if (r?.opened) { toast(`${name} 터미널을 열었습니다${r.terminal ? ` (${r.terminal})` : ""}`, 2000); return; }
    if (r?.client) { toast(`${name} 은 이미 열린 tmux 창에 보입니다`, 2500); return; }
    if (r?.command) {
      dialog(`<h3>터미널 열기 · ${esc(name)}</h3><p>터미널 프로그램을 열지 못했습니다${r.reason ? ` <span class="muted small">(${esc(r.reason)})</span>` : ""}. 터미널에서 아래 명령을 실행하면 이 에이전트 화면에 붙습니다.</p>
        <div class="cmd-copy"><code>${esc(r.command)}</code><button type="button" data-copy="${esc(r.command)}">복사</button></div>`, "닫기", () => {});
      return;
    }
    toast(`${name} 터미널을 열지 못했습니다`, 4000);
  }
  document.addEventListener("click", async (e) => {
    const b = e.target.closest("[data-copy]");
    if (!b) return;
    e.preventDefault();
    try { await navigator.clipboard.writeText(b.dataset.copy); }
    catch { const ta = document.createElement("textarea"); ta.value = b.dataset.copy; document.body.appendChild(ta); ta.select(); document.execCommand("copy"); ta.remove(); }
    b.textContent = "복사됨"; setTimeout(() => { if (b.isConnected) b.textContent = "복사"; }, 1500);
  });
  const renameDlg = (name) => dialog(`<h3>이름 바꾸기</h3><input type="text" name="name" value="${esc(name)}" required>`, "바꾸기", (fd) => {
    const nn = String(fd.get("name")).trim();
    if (nn && nn !== name) act("POST", `/api/agents/${enc(name)}/rename`, { name: nn }).then((r) => { if (r && ov.name === name) ov.name = nn; });
  });
  function askDlg(from) {
    const opts = state.agents.filter((a) => a.name !== from).map((a) => `<option>${esc(a.name)}</option>`).join("");
    dialog(`<h3>검토 요청</h3><div class="row">보내는 쪽 <select name="from"><option value="">사람(나)</option>${state.agents.map((a) => `<option ${a.name === from ? "selected" : ""}>${esc(a.name)}</option>`).join("")}</select>
      → 받는 쪽 <select name="to" required>${opts}</select></div><textarea name="request" required placeholder="요청 내용"></textarea>`, "보내기",
      (fd) => { const request = String(fd.get("request")).trim(); if (request) act("POST", "/api/ask", { from: fd.get("from") || null, to: fd.get("to"), request }); });
  }
  function askNamesDlg() {
    const amb = (state.agents || []).filter((a) => a.ambiguous);
    dialog(`<h3>모호한 이름 일괄 묻기</h3><p>이름이 모호한 에이전트${amb.length ? ` ${amb.length}개 (${amb.map((a) => esc(a.name)).join(", ")})` : ""}에게 스스로 이름을 정하라는 한 줄 요청을 보냅니다. 작업 중이거나 입력 중인 에이전트는 건너뜁니다.</p>`, "보내기", async () => {
      const r = await act("POST", "/api/request-names", {});
      if (!r) return;
      const q = r.queued || [], s = r.skipped || [];
      toast(`대기열 ${q.length}: ${q.join(", ") || "-"}\n건너뜀 ${s.length}: ${s.map((x) => `${x.name}(${x.reason})`).join(", ") || "-"}`, 9000);
    });
  }
  const interruptDlg = (name) => dialog(`<h3>${esc(name)} 중단</h3><p>진행 중인 작업을 중단(인터럽트)합니다. 계속할까요?</p>`, "중단", () => act("POST", `/api/agents/${enc(name)}/interrupt`, undefined, "중단 요청 보냄"));
  // Chat header (v46): buttons never wrap; when the row is too narrow the lowest-priority items fold into the ⋯ menu
  // (their menu entries are copied there). Info panel: below 900 px it folds behind an 정보 toggle (overlay).
  const OV_FOLD = [".ov-keys", '[data-pop="pop-name"]', '[data-k="propose"]', '[data-pop="pop-review"]', '[data-pop="pop-snooze"]'];
  function fitOvHead() {
    const ovEl = $(".ov"), head = $(".ov-head"), more = $("#pop-agent");
    if (!ovEl || !head) return;
    const mobile = isMobile();
    ovEl.classList.toggle("narrow", !mobile && ovEl.clientWidth < 900);
    if (!ovEl.classList.contains("narrow")) ovEl.classList.remove("side-open");
    const items = OV_FOLD.map((s) => { const el = head.querySelector(s); return el?.closest(".menu-wrap") || el; }).filter(Boolean);
    items.forEach((el) => el.classList.remove("fold"));
    more.querySelectorAll(".fold-mirror").forEach((x) => x.remove());
    if (mobile) return;
    const folded = [];
    for (const el of items) { if (head.scrollWidth <= head.clientWidth + 1) break; el.classList.add("fold"); folded.push(el); }
    for (const el of folded) {
      if (el.classList.contains("ov-keys")) continue;
      const pop = el.querySelector(".pop"), btn = el.matches("button") ? el : el.querySelector("[data-pop]");
      const label = (btn?.textContent || "").replace("▾", "").trim();
      const box = document.createElement("div"); box.className = "fold-mirror"; box.style.display = "contents";
      if (pop) box.innerHTML = `<div class="fold-mirror-h">${esc(label)}</div>${[...pop.children].filter((c) => !c.hidden).map((c) => c.outerHTML.replace(/\sid="[^"]*"/g, "")).join("")}`;
      else { const c = btn.cloneNode(false); c.removeAttribute("title"); c.classList.remove("fold"); c.innerHTML = `<b>${esc(label)}</b>`; box.innerHTML = `<div class="fold-mirror-h"></div>`; box.append(c); }
      more.append(box);
    }
  }
  new ResizeObserver(() => fitOvHead()).observe($(".ov"));
  new ResizeObserver(() => fitOvHead()).observe($(".ov-head"));

  // Pause the working marks when not visible: hidden tab (body.anim-off) or scrolled out of view (.wk.off).
  document.addEventListener("visibilitychange", () => document.body.classList.toggle("anim-off", document.hidden));
  const wkIO = "IntersectionObserver" in window ? new IntersectionObserver((es) => es.forEach((e) => e.target.classList.toggle("off", !e.isIntersecting))) : null;
  const observeWk = () => { if (!wkIO) return; wkIO.disconnect(); document.querySelectorAll(".wk").forEach((el) => wkIO.observe(el)); };
  new MutationObserver(() => { clearTimeout(observeWk.t); observeWk.t = setTimeout(observeWk, 50); }).observe(document.body, { childList: true, subtree: true });
  // Unsnooze from the board: highlights come back at once (optimistic), the next /api/state confirms.
  async function unsnooze(name) {
    try { await api("POST", `/api/agents/${enc(name)}/snooze`, { minutes: 0 }); } catch { return; }
    const s = agentBy(name); if (s) s.snoozedUntil = null;
    toast(L2(`${name} 미루기 해제`, `${name} unsnoozed`), 2000);
    if (!FIXTURE) refresh(); else render();
  }
  function snoozeCustomDlg(name) {
    dialog(`<h3>${esc(L2(`미루기 · ${name}`, `Snooze · ${name}`))}</h3>${snzCustomHtml("")}<p class="muted small">${L2("1분 이상 30일 이하", "1 minute to 30 days")}</p>`, L2("미루기", "Snooze"), () => {
      const m = snzCustomMin($("#dlg-form .snz-custom")); if (!m) return;
      act("POST", `/api/agents/${enc(name)}/snooze`, { minutes: m }, L2(`${snzLabel(m)} 미룸`, `snoozed ${snzLabel(m)}`));
    });
    const ok = $('#dlg-form button[value="ok"]'); if (ok) ok.disabled = true;
    setTimeout(() => $("#dlg-form .snz-n")?.focus(), 0);
  }
  // Custom snooze row: the apply button carries the minutes; invalid input disables it; Enter applies.
  function snzSync(box) {
    const m = snzCustomMin(box), go = box.querySelector(".snz-go") || box.closest("form")?.querySelector('button[value="ok"]');
    if (go) { go.disabled = !m; go.dataset.min = m || ""; }
    return go && m ? go : null;
  }
  document.addEventListener("input", (e) => { const box = e.target.closest?.(".snz-custom"); if (box) snzSync(box); });
  document.addEventListener("change", (e) => { const box = e.target.closest?.(".snz-custom"); if (box) snzSync(box); });
  document.addEventListener("keydown", (e) => {
    const box = e.key === "Enter" && !e.isComposing && e.target.closest?.(".snz-custom"); if (!box) return;
    e.preventDefault(); e.stopPropagation(); snzSync(box)?.click();
  }, true);
  $("#ov-unsnooze").insertAdjacentHTML("beforebegin", snzMenuHtml("snooze"));
  // ---------- events ----------
  document.addEventListener("click", (e) => {
    if (!$("#viewer").hidden && (e.target.closest(".viewer-close") || e.target === $("#viewer") || e.target.closest("#viewer img"))) return closeViewer();
    const img = e.target.closest("img.md-img");
    if (img) return openViewer(`<img src="${esc(img.getAttribute("src"))}" alt="${esc(img.dataset.path || "")}">`);
    const loc = e.target.closest("a[data-local]");
    if (loc) { e.preventDefault(); return openLocal(loc); }
    const cp = e.target.closest("[data-act=copy-code]");
    if (cp) {
      const code = cp.parentElement.querySelector("pre")?.textContent || "";
      navigator.clipboard?.writeText(code).then(() => { cp.textContent = t_("복사됨"); setTimeout(() => (cp.textContent = t_("복사")), 1200); }, () => toast(t_("복사 실패")));
      return;
    }
    const to = e.target.closest("[data-toggle-offline]");
    if (to) { const n = to.dataset.toggleOffline; expandedOffline.has(n) ? expandedOffline.delete(n) : expandedOffline.add(n); return render(); }
    const ga = e.target.closest("[data-launch-proj]");
    if (ga) return openLaunch(ga.dataset.launchProj);
    const pl = e.target.closest("a.pathlink");
    if (pl) { e.preventDefault(); return openPath(pl, e.altKey); }
    const om = e.target.closest("[data-open-with]");
    if (om) { const l = openMenu.link; closeOpenMenu(); return openPath(l, false, om.dataset.openWith); }
    const ro = e.target.closest("[data-agent-open]");
    if (ro) return openChat(ro.dataset.agentOpen);
    const hb = e.target.closest("[data-hours]");
    if (hb) { hours = +hb.dataset.hours; localStorage.setItem("adash.hours", hours); render(); return refreshHistory(); }
    const mpb = e.target.closest("[data-model-pick]");
    if (mpb) return openModelPop(mpb.dataset.modelPick, mpb);
    if ($("#model-pop") && !$("#model-pop").hidden && !e.composedPath().includes($("#model-pop"))) closeModelPop(); // composedPath: the pop re-renders on click, detaching e.target
    const us = e.target.closest("[data-unsnooze]");
    if (us) return unsnooze(us.dataset.unsnooze);
    const rs = e.target.closest('[data-act="restart-agent"][data-name]');
    if (rs) return restartAgent(rs.dataset.name);
    const row = e.target.closest(".lrow[data-agent]");
    if (row && (e.ctrlKey || e.metaKey || e.shiftKey || e.target.classList.contains("pick") || bulk.size)) { e.preventDefault(); return toggleBulk(row.dataset.agent, e.shiftKey); }
    if (row) return openChat(row.dataset.agent);
    if (e.target.closest("#timeline")) { const n = rowAt(e); if (n) return openChat(n); return; }
    const ot = e.target.closest("[data-open-tab]");
    if (ot) return openChat(ov.open ? ov.name : null, ot.dataset.openTab);
    const t = e.target.closest(".tabs [data-tab]");
    if (t) { ov.tab = t.dataset.tab; if (ov.tab === "meta") loadMeta(); return render(); }
    if (e.target.closest("#ov-close")) return closeChat();
    if (e.target === $("#overlay")) return closeChat();
    if (e.target.closest("#new-pill")) { const log = $("#chat-log"); log.scrollTop = log.scrollHeight; $("#new-pill").hidden = true; return; }
    if (e.target.closest("#ask-names")) return askNamesDlg();
    if (e.target.id === "meta-save") return saveMeta();
    const b = e.target.closest("[data-act]");
    if (!b) return;
    const { act: a, id } = b.dataset;
    // Chat-header actions carry no data-name: they act on the open chat.
    const name = b.dataset.name ?? (b.closest(".ov-head") ? ov.name : undefined);
    if (a === "focus") openTerminal(name);
    else if (a === "ask") askDlg(name);
    else if (a === "rename") renameDlg(name);
    else if (a === "request-name") act("POST", `/api/agents/${enc(name)}/request-name`, {}, "이름 요청 보냄");
    else if (a === "snooze") {
      const minutes = +b.dataset.min || 0;
      act("POST", `/api/agents/${enc(name)}/snooze`, { minutes }, minutes ? L2(`${snzLabel(minutes)} 미룸`, `snoozed ${snzLabel(minutes)}`) : L2("미루기 해제", "unsnoozed"));
    }
    else if (a === "interrupt") interruptDlg(name);
    else if (a === "ask-skip") askSkipDlg(ov.name);
    else if (a === "side-tog") { $(".ov").classList.toggle("side-open"); fitOvHead(); }
    else if (a === "od-all") odDialog(null);
    else if (a === "od-one") odDialog([name]);
    else if (a === "send-force") sendComposer(true);
    else if (a === "pending-drop" || a === "pending-resend") {
      const p = ov.pending[+b.dataset.i]; if (!p) return;
      ov.pending.splice(+b.dataset.i, 1); p.urls.forEach((u) => URL.revokeObjectURL(u));
      // Send again: the text goes back through the normal send (attachments are not re-attached; say so).
      if (a === "pending-resend") { $("#composer-text").value = p.text; if (p.images.length || p.files?.length) toast(L2("첨부는 다시 붙여 주세요", "Re-attach the files"), 3000); else sendComposer(); }
      renderChat();
    }
    else if (a === "held-cancel") { ov.held = null; renderHeld(); $("#composer-text").focus(); }
    else if (a === "ask-pick") askPick(b.dataset.q, +b.dataset.i);
    else if (a === "ask-submit") submitAsk();
    else if (a === "dec-opt") setComposerItem(+b.dataset.n, b.dataset.o);
    else if (a === "dec-item") setComposerItem(+b.dataset.n, composerItem(+b.dataset.n) || "");
    else if (a === "dec-all") answerAllDecisions();
    else if (a === "dec-fold") toggleDecFold();
    else if (a === "draft-submit") draftAction("submit");
    else if (a === "draft-clear") dialog(`<h3>초안 지우기</h3><p>터미널 입력창에 남아 있는 초안을 지웁니다. 되돌릴 수 없습니다.</p>`, "지우기", () => draftAction("clear"));
    else if (a === "draft-import") {
      const d = agentBy(ov.name)?.draft?.text || "", ta = $("#composer-text");
      ta.value = ta.value.trim() ? `${ta.value.replace(/\s+$/, "")}\n${d}` : d;
      ta.focus();
      if (!ov.draftNoClear.has(ov.name)) dialog(`<h3>터미널 입력창 초안도 지울까요?</h3><p>내용은 대시보드 입력창으로 옮겼습니다. 터미널 쪽 초안을 지우지 않으면 다음 전송 때 함께 제출될 수 있습니다.</p>`, "지우기", () => draftAction("clear"));
    }
    else if (a === "view") setView(b.dataset.v);
    else if (a === "btw-close") btwCall(ov.name, "close");
    else if (a === "net-details") netDetails();
    else if (a === "recheck") { refresh(); refreshHistory(); }
    else if (a === "filter-all") { filter = "all"; localStorage.setItem("adash.filter", filter); render(); }
    else if (a === "close-agent") closeAgentsDlg([name], b.dataset.mode);
    else if (a === "reopen-agent") reopenAgent(name);
    else if (a === "restart-agent") restartAgent(b.dataset.name || name);
    else if (a === "resume") {
      const ta = $("#composer-text"), v = ta.value.replace(/\s+$/, "");
      ta.value = v ? `${v}\n계속 진행해` : "계속 진행해";
      ta.focus(); ta.setSelectionRange(ta.value.length, ta.value.length);
    }
    else if (a === "m-new") openLaunch();
    else if (a === "m-sessions") openSessions();
    else if (a === "m-reports") (typeof openReports === "function" ? openReports() : toast("보고서는 아직 없습니다"));
    else if (a === "m-push") openPushSettings();
    else if (a === "dev-approve" || a === "dev-revoke") { $("#dlg").close(); act("POST", `/api/devices/${enc(id)}/${a === "dev-approve" ? "approve" : "revoke"}`, {}, a === "dev-approve" ? "기기를 승인했습니다" : "기기를 해제했습니다"); }
    else if (a === "magi") openMagi(name || ov.name, b.dataset.k);
    else if (a === "aj-close") { answerJobs.delete(ov.name); renderAskCard(); render(); }
    else if (a === "aj-retry") { const j = answerJobs.get(ov.name); answerJobs.delete(ov.name); if (j?.body) answerAsk(j.body); }
    else if (a === "magi-deliver" || a === "magi-cancel") {
      if (a === "magi-cancel") dialog(`<h3>magi 요청 취소</h3><p>${esc(id)} 요청을 취소합니다. 이미 대상에게 간 질문은 되돌리지 않습니다.</p>`, "취소하기", () => magiAction(id, "cancel"));
      else magiAction(id, "deliver");
    }
    else if (a === "magi-launch") { $("#dlg").close(); const p = b.dataset.proj; openLaunch(p, MAGI_PRESET[b.dataset.role](p)); }
    else if (a?.startsWith("bulk-")) bulkAction(a.slice(5), b);
    else if (a === "deliver") act("POST", `/api/threads/${enc(id)}/deliver-anyway`);
    else if (a === "relay-force") {
      dialog(`<h3>초안과 함께 전달</h3><p>${esc(b.dataset.to)}의 검토 답변 안내 한 줄을 지금 보냅니다. 요청한 에이전트의 입력창에 남아 있는 초안이 이 줄과 함께 제출됩니다. 계속할까요?</p>`, "전달", () => act("POST", `/api/threads/${enc(id)}/deliver-anyway`, undefined, "전달 요청 보냄"));
    } else if (a === "open-reply") {
      const dir = b.dataset.dir;
      if (!dir) return toast("스레드 폴더를 알 수 없습니다");
      const link = document.createElement("a");
      link.dataset.local = `${dir}/reply.md`; link.dataset.agent = ov.name; link.setAttribute("href", fileUrl(ov.name, `${dir}/reply.md`, Date.now()));
      openLocal(link);
    }
    else if (a === "task-done") act("PATCH", `/api/human-tasks/${enc(id)}`, { done: b.type === "checkbox" ? b.checked : true });
  });
  // btw follow-up row under the latest btw answer (Enter submits; IME composition is left alone).
  $("#chat-log").addEventListener("submit", (e) => {
    const f = e.target.closest("[data-btw-follow]");
    if (!f) return;
    e.preventDefault();
    const q = f.querySelector("input[name=q]"), text = q.value.trim();
    if (!text) return;
    btwCall(ov.name, "ask", text).then((ok) => { if (ok) q.value = ""; });
  });
  $("#ask-card").addEventListener("input", (e) => { if (e.target.classList.contains("ask-text")) askTextChanged(e.target); });
  $("#ask-card").addEventListener("keydown", (e) => {
    if (e.target.classList.contains("ask-text") && e.key === "Enter" && !e.isComposing) { e.preventDefault(); if (enterSends() || e.ctrlKey || e.metaKey) submitAsk(); }
    // Option cards are role=button divs: Enter/Space pick them like buttons.
    else if ((e.key === "Enter" || e.key === " ") && e.target.matches?.('.ask-opt[data-act="ask-pick"]')) { e.preventDefault(); e.stopPropagation(); e.target.click(); }
  });
  $("#chat-log").addEventListener("toggle", (e) => {
    const g = e.target.dataset?.gid; if (!g) return;
    e.target.open ? ov.openGroups.add(g) : ov.openGroups.delete(g);
  }, true);
  // Missing local image: replace the broken icon with the path it pointed to.
  document.addEventListener("error", (e) => {
    const img = e.target;
    if (!(img instanceof HTMLImageElement) || !img.classList.contains("md-img")) return;
    const span = document.createElement("span");
    span.className = "img-missing";
    span.textContent = `${L2("이미지를 불러올 수 없음:", "Image not available:")} ${img.dataset.path || img.getAttribute("src")}`;
    img.replaceWith(span);
  }, true);

  // ---------- viewer (image lightbox / local Markdown document) ----------
  function openViewer(html, wide) {
    const v = $("#viewer");
    v.querySelector(".viewer-body").innerHTML = html;
    v.classList.toggle("doc", !!wide);
    v.hidden = false;
  }
  const closeViewer = () => { $("#viewer").hidden = true; $("#viewer .viewer-body").innerHTML = ""; };
  async function openLocal(a) {
    const p = a.dataset.local, href = a.getAttribute("href");
    const ext = (p.split(/[?#]/)[0].match(/\.([a-z0-9]+)$/i) || [])[1]?.toLowerCase();
    if (["png", "jpg", "jpeg", "gif", "webp", "svg"].includes(ext)) return openViewer(`<img src="${esc(href)}" alt="${esc(p)}">`);
    if (ext === "pdf") return openViewer(`<div class="doc-path"><code class="path">${esc(p)}</code></div><iframe class="pdf-view" src="${esc(href)}" title="${esc(baseName(p))}"></iframe>`, true);
    if (!["md", "markdown", "txt"].includes(ext)) return window.open(href, "_blank", "noopener");
    openViewer(`<p class="muted">불러오는 중… ${esc(p)}</p>`, true);
    try {
      const r = await fetch(href);
      if (!r.ok) { let m = `HTTP ${r.status}`; try { m = (await r.json()).error || m; } catch { /* not JSON */ } throw new Error(m); }
      const text = await r.text();
      openViewer(`<div class="doc-path"><code class="path">${esc(p)}</code></div><div class="md">${ext === "txt" ? `<pre>${esc(text)}</pre>` : renderMd(text, { agent: a.dataset.agent, v: Date.now() })}</div>`, true);
      if (ext !== "txt") enhanceRich($("#viewer"));
    } catch (err) { openViewer(`<p>문서를 열 수 없음: <code class="path">${esc(p)}</code></p><p class="muted">${esc(err.message)}</p>`, true); }
  }
  $("#chat-log").addEventListener("scroll", (e) => {
    const l = e.target;
    if (l.scrollHeight - l.scrollTop - l.clientHeight < 80) $("#new-pill").hidden = true;
  });
  $("#composer").addEventListener("submit", (e) => { e.preventDefault(); sendComposer(false); });
  // Attachments (any file): 첨부 picker, Ctrl+V from the clipboard, or drop anywhere on the chat column.
  $("#att-btn").addEventListener("click", () => $("#att-input").click());
  $("#filter-sel").addEventListener("change", (e) => { filter = e.target.value; localStorage.setItem("adash.filter", filter); render(); });
  $("#sort-sel").addEventListener("change", (e) => { sortMode = e.target.value; localStorage.setItem("adash.sort", sortMode); render(); });
  $("#att-input").addEventListener("change", (e) => { addFiles([...e.target.files]); e.target.value = ""; });
  $("#att-bar").addEventListener("click", (e) => {
    const rt = e.target.closest("[data-att-retry]");
    if (rt) { const a = ov.atts.find((x) => x.id === +rt.dataset.attRetry); if (a?.file) startUpload(a, ov.name); else if (a?.local) { ov.atts = ov.atts.filter((x) => x !== a); addLocalPaths([a.path]); } return; }
    const b = e.target.closest("[data-att-rm]"); if (!b) return;
    const a = ov.atts.find((x) => x.id === +b.dataset.attRm);
    if (a) { if (a.url) URL.revokeObjectURL(a.url); ov.atts = ov.atts.filter((x) => x !== a); renderAtts(); }
  });
  document.addEventListener("paste", (e) => {
    if (!ov.open || !ov.name) return;
    const files = [...(e.clipboardData?.files || [])];
    if (files.length) { e.preventDefault(); addFiles(files); return; }
    const local = fileUriPaths(e.clipboardData?.getData("text/uri-list") || e.clipboardData?.getData("text/plain"));
    if (local) { e.preventDefault(); addLocalPaths(local); }
  });
  const chatCol = $(".chat");
  chatCol.addEventListener("dragover", (e) => { if ([...e.dataTransfer.types].includes("Files")) { e.preventDefault(); chatCol.classList.add("drop"); } });
  chatCol.addEventListener("dragleave", (e) => { if (!chatCol.contains(e.relatedTarget)) chatCol.classList.remove("drop"); });
  chatCol.addEventListener("drop", (e) => { chatCol.classList.remove("drop"); if (!e.dataTransfer.files.length) return; e.preventDefault(); addFiles([...e.dataTransfer.files]); });
  // IME-safe: while a Korean syllable is being composed the menu keeps its state; it refilters on compositionend.
  $("#composer-text").addEventListener("input", (e) => { if (decisionsOf(agentBy(ov.name)).length) renderDecisions(); if (!e.isComposing) updateSlash(); updatePrefixWarn(); });
  $("#composer-text").addEventListener("compositionend", () => updateSlash());
  $("#composer-text").addEventListener("click", () => (slash.open ? updateSlash() : null));
  $("#composer-text").addEventListener("blur", () => setTimeout(() => { if (document.activeElement !== $("#composer-text")) closeSlash(); }, 0));
  $("#composer-text").addEventListener("keydown", (e) => {
    if (slashKey(e)) return;
    // Enter sends only where enabled (desktop default); on touch it stays a newline. Ctrl/Cmd+Enter always force-sends.
    if (e.key === "Enter" && !e.isComposing && ((e.ctrlKey || e.metaKey) || (!e.shiftKey && enterSends()))) { e.preventDefault(); sendComposer(e.ctrlKey || e.metaKey); }
  });
  $("#ht-form").addEventListener("submit", (e) => {
    e.preventDefault();
    const fd = new FormData(e.target);
    act("POST", "/api/human-tasks", { title: String(fd.get("title")).trim(), project: fd.get("project") || null, due: fd.get("due") || null });
    e.target.reset();
  });

  document.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") { e.preventDefault(); return $("#palette").hidden ? openPalette() : closePalette(); }
    // IME composition (Korean etc.): never treat keys as shortcuts.
    if (e.isComposing || e.keyCode === 229 || e.key === "Process") return;
    if (!$("#palette").hidden) return palKey(e);
    if ($("#dlg").open || $("#launch-dlg").open || !state) return;
    if (e.key === "Escape" && !$("#font-panel").hidden) { e.preventDefault(); $("#font-panel").hidden = true; return; }
    if (e.altKey && ov.open && ["=", "+", "-", "0"].includes(e.key)) { e.preventDefault(); return e.key === "0" ? (fontCfg.chat = FONT_DEFAULTS.chat, applyFont()) : bumpChat(e.key === "-" ? -1 : 1); }
    if (e.key === "Escape" && !$("#viewer").hidden) { e.preventDefault(); return closeViewer(); }
    if (e.key === "Escape" && ov.open) { e.preventDefault(); return closeChat(); }
    // An answer is being typed into the agent's prompt: no navigation/ack/focus keys until it ends.
    if (document.body.classList.contains("answering")) return;
    // Typing anywhere (ask "기타" field, composer, search, forms): no single-key shortcuts.
    if (isEditable(e.target) || isEditable(document.activeElement) || e.ctrlKey || e.metaKey || e.altKey) return;
    if (ov.open) {
      if (e.key === "[") { e.preventDefault(); switchWaiting(-1); }
      else if (e.key === "]") { e.preventDefault(); switchWaiting(1); }
      else if (e.key === "e") { e.preventDefault(); ackAndNext(ov.name); }
      else if (e.key === "r") { e.preventDefault(); $("#composer-text").focus(); }
      return;
    }
    const order = navOrder();
    if (["j", "k", "ArrowDown", "ArrowUp"].includes(e.key)) {
      if (!order.length) return;
      e.preventDefault();
      const i = order.indexOf(selName);
      const fwd = ["j", "ArrowDown"].includes(e.key);
      selName = order[i < 0 ? 0 : (i + (fwd ? 1 : -1) + order.length) % order.length];
      render();
      document.querySelector(".lrow.sel")?.scrollIntoView({ block: "nearest" });
    } else if (e.key === "Enter") {
      const n = selName || order[0];
      if (n) { e.preventDefault(); openChat(n); }
    } else if (e.key === "e") {
      if (selName) { e.preventDefault(); ackAndNext(selName); }
    } else if (e.key === "o") {
      if (selName) openTerminal(selName);
    } else if (e.key === "n") {
      e.preventDefault(); openLaunch();
    }
  });

  // ---------- mobile layout (< 768 px): bottom tabs 내 차례 | 프로젝트 | 활동 | 더보기 (Phase C) ----------
  const mobileMq = matchMedia("(max-width: 767px)");
  const isMobile = () => mobileMq.matches;
  let mtab = localStorage.getItem("adash.mtab") || "mine";
  function applyMobile() {
    const on = isMobile();
    document.body.classList.toggle("mobile", on);
    $("#mobile").hidden = !on; $("#mtabs").hidden = !on;
    document.body.dataset.mtab = mtab;
    if (on && state) renderMobile();
  }
  mobileMq.addEventListener("change", applyMobile);
  const firstLine = (s) => String(s || "").split("\n").map((l) => l.replace(/^[#>*\-\s]+/, "").trim()).find(Boolean) || "";
  // 내 차례 cards: the same set the counts describe (item 8), in triage order.
  function mineList() {
    const order = triageOrder();
    const by = new Map(model.agents.map((a) => [a.name, a]));
    const rest = model.agents.filter((a) => a.turn && !order.includes(a.name));
    return [...order.map((n) => by.get(n)).filter(Boolean), ...rest];
  }
  function stateLine(a) {
    if (a.kind === "asking") return `<span class="ask-n">질문</span> · ${fmtMin(a.waitedMin) || "0분"}`;
    if (a.kind === "error" && a.s?.state === "auth") return `<span class="err-n">${t_("로그인 필요")}</span>`;
    if (a.kind === "error") return `<span class="err-n">${esc(errLabel(a.s?.lastError))}</span>${a.s?.lastError?.at ? ` · ${ago(a.s.lastError.at)}` : ""}`;
    if (a.kind === "waiting") return `${a.unread ? '<span class="unread-n">미확인</span> · ' : ""}${fmtMin(a.waitedMin)} 전 답`;
    if (a.kind === "working") return `작업 중${a.s?.tool ? ` · ${esc(a.s.tool)}` : ""}`;
    return SEG_KO[a.kind] || a.kind;
  }
  function sparkline(a) {
    // Simplified history: one thin bar per window, colored by state (no Gantt on phones).
    const { from, now: to } = model, span = to - from || 1;
    return `<svg class="spark" viewBox="0 0 100 6" preserveAspectRatio="none">${a.segs.filter((s) => s.state !== "offline" && s.end > from).map((s) => `<rect class="s-${s.state === "waiting" ? "wait1" : s.state}" x="${(((Math.max(s.start, from) - from) / span) * 100).toFixed(2)}" y="1" width="${Math.max(0.4, ((s.end - Math.max(s.start, from)) / span) * 100).toFixed(2)}" height="4" rx="2"></rect>`).join("")}</svg>`;
  }
  function renderMobile() {
    document.body.dataset.mtab = mtab;
    $$("[data-mtab]").forEach((b) => b.classList.toggle("on", b.dataset.mtab === mtab));
    const mine = mineList();
    $("#mt-mine").textContent = model.counts.mine ? ` ${model.counts.mine}` : "";
    let html = "";
    if (mtab === "mine") {
      html = mine.length ? mine.map((a) => {
        const s = a.s, dn = decisionsOf(s).length;
        return `<div class="mcard${a.unread ? " unread" : ""}${a.kind === "asking" ? " asking" : ""}${a.kind === "error" ? " erred" : ""}" data-magent="${esc(a.name)}">
          <div class="mc-swipe"><span class="mc-l">확인함</span><span class="spacer"></span><span class="mc-r">${L2("미루기 1시간", "Snooze 1 h")}</span></div>
          <div class="mc-body"><div class="mc-head"><b>${esc(a.name)}</b>${a.kind === "working" ? WKa(a.name) : isMyTurn(a) ? TK(a) : ""}<span class="muted mc-proj">${esc(a.project)}</span><span class="spacer"></span><span class="mc-st">${stateLine(a)}</span></div>
            <div class="mc-meta">${a.role ? `<span class="muted">${esc(a.role)}</span>` : ""}${dn ? `<span class="dec-mark">결정 ${dn}</span>` : ""}${heldRelays(a.name).length ? '<span class="held-mark">답변 보류</span>' : ""}${s?.draft?.text ? '<span class="draft-mark">초안</span>' : ""}${soonCold(s) ? `<span class="cd soon" title="${esc(cacheBasis(s))}">곧 식음</span>` : ""}</div>
            <div class="mc-first">${esc(a.kind === "error" ? (s?.state === "auth" ? t_("로그인이 필요합니다") : s?.lastError?.message || "제공자 API 오류로 멈춤") : a.kind === "asking" && s?.ask ? askQuestions(s.ask)[0].question : firstLine(s?.lastMessage))}</div>${a.kind === "error" ? `<div class="mc-act"><button data-retry="${esc(a.name)}">${esc(retryLabel(s))}</button></div>` : ""}</div></div>`;
      }).join("") : `<p class="muted m-empty">기다리는 에이전트가 없습니다</p>`;
    } else if (mtab === "projects") {
      html = model.groups.map((g) => `<section class="mproj"><div class="mp-head"><b>${esc(g.name)}</b>${g.asking ? `<span class="ask-n">질문 ${g.asking}</span>` : ""}${g.unread ? `<span class="unread-n">미확인 ${g.unread}</span>` : ""}<span class="spacer"></span><span class="muted small">에이전트 ${g.allCount}</span><button class="g-more m" data-proj-menu="${esc(g.name)}" aria-label="프로젝트 메뉴">⋯</button></div>
        ${g.members.map((a) => `<div class="mp-row" data-magent="${esc(a.name)}">${harnessBadge(a.s, a.live)}<span class="mp-n${a.unread ? " unread" : ""}"><span class="mp-nt">${esc(a.name)}</span>${a.kind === "working" ? WKa(a.name) : isMyTurn(a) ? TK(a) : ""}${a.live ? netIcon(a.s) : ""}</span><span class="mp-st muted">${a.live ? stateLine(a) : `<button class="reopen-btn m" data-reopen="${esc(a.name)}">다시 열기</button>`}</span>${sparkline(a)}</div>`).join("")}${g.hidden.length ? `<div class="mp-row mp-off muted small" data-toggle-offline="${esc(g.name)}">${expandedOffline.has(g.name) ? "오프라인 접기" : `오프라인 ${g.hidden.length}개 숨김`}</div>` : ""}</section>`).join("") + hiddenProjHtml(true);
    } else if (mtab === "more") {
      html = `${capListHtml()}<div class="mmore"><button class="primary" data-act="m-new">새 세션</button>${odAgents().length ? `<button data-act="od-all">↑ ${L2(`업데이트된 세션 ${odAgents().length} · 모두 재시작`, `${odAgents().length} updated · restart all`)}</button>` : ""}<button data-act="m-sessions">지난 세션</button><button data-act="m-reports">보고서</button><button data-act="m-push">알림·기기 설정</button><button data-open-tab="threads">검토 요청 목록</button><button data-open-tab="tasks">내 할 일</button><button class="font-btn">표시 설정</button></div>`;
    }
    H("#mobile").innerHTML = html;
  }
  // Swipe a "내 차례" card: right = 확인함, left = 미루기 60분; a tap opens the chat.
  {
    let sw = null;
    const el = $("#mobile");
    el.addEventListener("touchstart", (e) => { const c = e.target.closest(".mcard"); if (!c) return; sw = { c, x: e.touches[0].clientX, y: e.touches[0].clientY, dx: 0, lock: null }; }, { passive: true });
    el.addEventListener("touchmove", (e) => {
      if (!sw) return;
      const dx = e.touches[0].clientX - sw.x, dy = e.touches[0].clientY - sw.y;
      if (sw.lock == null && Math.abs(dx) + Math.abs(dy) > 8) sw.lock = Math.abs(dx) > Math.abs(dy) ? "x" : "y";
      if (sw.lock !== "x") return;
      sw.dx = dx;
      sw.c.querySelector(".mc-body").style.transform = `translateX(${dx}px)`;
      sw.c.classList.toggle("go-r", dx > 80); sw.c.classList.toggle("go-l", dx < -80);
    }, { passive: true });
    // Commit the swipe at once: the card slides out and the toast shows before the request
    // returns; a failed request brings the card back with an error toast.
    el.addEventListener("touchend", () => {
      if (!sw) return;
      const { c, dx, lock } = sw; sw = null;
      const body = c.querySelector(".mc-body");
      c.classList.remove("go-r", "go-l");
      // only a horizontal swipe swallows the click that follows; a tap still opens the chat
      if (lock === "x") { c.dataset.swiped = "1"; setTimeout(() => delete c.dataset.swiped, 300); }
      const act = lock === "x" ? (dx > 80 ? "ack" : dx < -80 ? "snooze" : null) : null;
      if (!act) { body.style.transition = "transform .18s ease-out"; body.style.transform = ""; setTimeout(() => (body.style.transition = ""), 200); return; }
      const name = c.dataset.magent, s = agentBy(name);
      const prev = s ? { unread: s.unread, snoozedUntil: s.snoozedUntil } : null;
      body.style.transition = "transform .16s ease-in";
      body.style.transform = `translateX(${dx > 0 ? "" : "-"}110%)`;
      c.style.transition = "height .16s ease-in .12s, opacity .16s ease-in .12s";
      c.style.height = `${c.offsetHeight}px`; c.style.overflow = "hidden";
      requestAnimationFrame(() => { c.style.height = "0px"; c.style.opacity = "0"; });
      if (s) { if (act === "ack") s.unread = false; else s.snoozedUntil = Date.now() + 3600e3; }
      toast(act === "ack" ? `${name} 확인함` : L2(`${name} 1시간 미룸`, `${name} snoozed 1 h`), 1500);
      setTimeout(render, 300);
      api("POST", `/api/agents/${enc(name)}/${act}`, act === "snooze" ? { minutes: 60 } : undefined, { quiet: true })
        .then(() => { if (!FIXTURE) refresh(); })
        .catch((err) => {
          if (s && prev) Object.assign(s, prev);
          toast(`${name}: ${act === "ack" ? "확인" : "미루기"} 실패 · ${err?.message || err}`, 3000);
          render();
        });
    });
  }
  $("#mobile").addEventListener("click", (e) => {
    const c = e.target.closest("[data-magent]");
    if (c && !c.dataset.swiped) openChat(c.dataset.magent);
  });
  $("#mtabs").addEventListener("click", (e) => {
    const b = e.target.closest("[data-mtab]"); if (!b) return;
    mtab = b.dataset.mtab; localStorage.setItem("adash.mtab", mtab);
    if (ov.open) closeChat();
    renderMobile(); window.scrollTo(0, 0);
  });

  // ---------- Web Push + devices (Phase C) ----------
  const PUSH_EVENTS = [["asking", "질문 대기"], ["human_answer", "내가 직접 물은 질문에 답이 옴"], ["relay_held", "검토 답변 보류"], ["escalated", "에스컬레이션"], ["waiting_after_human", "내가 말 건 에이전트가 답을 남김"]];
  const b64ToU8 = (s) => { const p = "=".repeat((4 - (s.length % 4)) % 4); const raw = atob((s + p).replace(/-/g, "+").replace(/_/g, "/")); return Uint8Array.from(raw, (c) => c.charCodeAt(0)); };
  async function pushState() {
    if (!("serviceWorker" in navigator) || !("PushManager" in window)) return { supported: false };
    const reg = await navigator.serviceWorker.getRegistration();
    const sub = reg ? await reg.pushManager.getSubscription() : null;
    return { supported: true, sub };
  }
  async function openPushSettings() {
    const ps = await pushState().catch(() => ({ supported: false }));
    let key = null, devices = [];
    try { key = await api("GET", "/api/push/key", undefined, { quiet: true }); } catch { /* push not configured */ }
    try { devices = (await api("GET", "/api/devices", undefined, { quiet: true }))?.devices || []; } catch { /* no device list */ }
    const cur = JSON.parse(localStorage.getItem("adash.push") || "{}");
    const evs = new Set(cur.events || PUSH_EVENTS.map((e) => e[0]));
    dialog(`<h3>알림·기기</h3>
      <div class="ps-sec"><b>이 기기 푸시 알림</b> <span class="muted small">${!ps.supported ? "이 브라우저는 푸시를 지원하지 않습니다" : !key?.publicKey ? "서버에 푸시 키가 없습니다" : ps.sub ? "켜짐" : "꺼짐"}</span>
        ${PUSH_EVENTS.map(([k, l]) => `<label class="mg-t"><input type="checkbox" name="ev" value="${k}" ${evs.has(k) ? "checked" : ""}>${l}</label>`).join("")}
        <div class="row">조용한 시간 <input type="time" name="qf" value="${esc(cur.quietHours?.from || "")}"> ~ <input type="time" name="qt" value="${esc(cur.quietHours?.to || "")}"></div>
        ${key?.idleSupported ? `<label class="mg-t"><input type="checkbox" name="idle" ${cur.onlyWhenIdle ? "checked" : ""}>데스크톱을 5분 이상 쓰지 않을 때만</label>` : ""}
        <label class="mg-t"><input type="checkbox" name="on" ${ps.sub ? "checked" : ""} ${ps.supported && key?.publicKey ? "" : "disabled"}>이 기기에서 푸시 받기</label></div>
      ${devices.length ? `<div class="ps-sec"><b>기기</b>${devices.map((d) => `<div class="dev-row"><span>${d.node ? `<b>${esc(d.node)}</b> ` : ""}${esc(d.name || d.id)}</span><span class="muted small">${esc(d.login || "")}${d.lastSeen ? ` · ${ago(d.lastSeen)}` : ""}</span><span class="spacer"></span>${d.approved ? `<button type="button" data-act="dev-revoke" data-id="${esc(d.id)}" title="${esc(d.node ? L2(`이 Tailscale 기기(${d.node})의 모든 접속을 해제`, `Revokes every login from this Tailscale device (${d.node})`) : "")}">해제</button>` : `<button type="button" class="primary" data-act="dev-approve" data-id="${esc(d.id)}">승인</button>`}</div>`).join("")}</div>` : ""}`, "저장", async (fd) => {
      const settings = { events: fd.getAll("ev"), quietHours: fd.get("qf") && fd.get("qt") ? { from: fd.get("qf"), to: fd.get("qt") } : null, onlyWhenIdle: !!fd.get("idle") };
      localStorage.setItem("adash.push", JSON.stringify(settings));
      try {
        const want = !!fd.get("on");
        if (want && key?.publicKey) {
          const reg = await navigator.serviceWorker.ready;
          const sub = ps.sub || await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64ToU8(key.publicKey) });
          await api("POST", "/api/push/subscribe", { subscription: sub.toJSON(), settings });
          toast("푸시 알림을 켰습니다", 2500);
        } else if (!want && ps.sub) {
          await api("POST", "/api/push/unsubscribe", { endpoint: ps.sub.endpoint }).catch(() => null);
          await ps.sub.unsubscribe();
          toast("푸시 알림을 껐습니다", 2500);
        }
      } catch (e) { toast(`푸시 설정 실패: ${e.message}`); }
    });
  }
  // A new phone (through Tailscale) waits until the desktop approves it.
  function showPairing(j) {
    const el = $("#pair-screen");
    el.hidden = false;
    el.innerHTML = `<div class="pair-box"><h2>이 기기 승인 대기</h2><p>데스크톱의 adash 대시보드에서 이 기기를 승인하세요.</p>${j.code ? `<p class="pair-code">${esc(j.code)}</p>` : ""}<p class="muted small">${esc(j.name || j.deviceId || "")}</p></div>`;
  }
  const hidePairing = () => { $("#pair-screen").hidden = true; };
  // Device approval prompts. A request is shown at most once per (id, createdAt) across reloads and tabs
  // (localStorage), only while it is really pending (not approved, not revoked), and an open prompt closes
  // itself as soon as the server reports the request as handled (approved here, in another tab, or revoked).
  const DEV_KEY = "adash.devPrompted";
  const devKey = (d) => `${d.id}@${d.createdAt || ""}`;
  const devPrompted = () => { try { return new Set(JSON.parse(localStorage.getItem(DEV_KEY) || "[]")); } catch { return new Set(); } };
  const markDevPrompted = (d) => { const s = devPrompted(); s.add(devKey(d)); localStorage.setItem(DEV_KEY, JSON.stringify([...s].slice(-50))); };
  let devPrompt = null; // id of the device whose prompt is open
  async function pollDevices() {
    if (FIXTURE || isMobile()) return;
    let ds = [];
    try { ds = (await api("GET", "/api/devices", undefined, { quiet: true }))?.devices || []; } catch { return; }
    const pending = ds.filter((d) => !d.approved && !d.revoked);
    if (devPrompt && !pending.some((d) => d.id === devPrompt)) {
      if ($("#dlg").open && $("#dlg").dataset.dev === devPrompt) $("#dlg").close();
      devPrompt = null;
    }
    if (devPrompt || $("#dlg").open) return;
    const seen = devPrompted();
    const p = pending.find((d) => !seen.has(devKey(d)));
    if (!p) return;
    markDevPrompted(p);
    devPrompt = p.id;
    dialog(`<h3>새 기기 승인 요청</h3><p>${p.node ? `<b>${esc(p.node)}</b> · ` : ""}<b>${esc(p.name || p.id)}</b> <span class="muted">${esc(p.login || "")}</span></p>${p.code ? `<p>휴대폰에 보이는 코드: <b class="pair-code">${esc(p.code)}</b></p>` : ""}<p class="muted small">모르는 기기라면 취소하세요. 나중에 '알림·기기 설정'에서 승인할 수 있습니다.</p>`, "승인", () => act("POST", `/api/devices/${enc(p.id)}/approve`, {}, "기기를 승인했습니다"));
    $("#dlg").dataset.dev = p.id;
    $("#dlg").addEventListener("close", () => { delete $("#dlg").dataset.dev; devPrompt = null; }, { once: true });
  }
  setInterval(pollDevices, 15000);
  if ("serviceWorker" in navigator && (location.protocol === "https:" || location.hostname === "127.0.0.1" || location.hostname === "localhost")) navigator.serviceWorker.register("sw.js").catch(() => null);
  navigator.serviceWorker?.addEventListener("message", (e) => { const u = e.data?.open && new URL(e.data.open, location.href); const n = u?.searchParams.get("agent"); if (n) openChat(n); });
  const startAgent = new URLSearchParams(location.search).get("agent");
  if (startAgent) { const t0 = setInterval(() => { if (state) { clearInterval(t0); openChat(startAgent); } }, 200); }
  applyMobile();

  // ---------- reports (Phase D): list, inline HTML (sandboxed iframe), PDF, make now ----------
  const rp = { list: [], sel: null, timer: null };
  const reportUrl = (id, pdf) => (FIXTURE ? "fixture_report.html" : `/api/reports/${enc(id)}${pdf ? "/pdf" : ""}`);
  const PERIOD_KO = { week: "주간", month: "월간", custom: "기간" };
  const HEADLINE_KO = { messages: "보낸 메시지", activeHours: "활동 시간", agents: "에이전트", turns: "턴", agentWorkHours: "에이전트 작업 시간", cost: "비용" };
  async function loadReports() {
    try { rp.list = (await api("GET", "/api/reports", undefined, { quiet: true }))?.reports || []; }
    catch (e) { rp.list = []; H("#rp-list").innerHTML = `<p class="muted small rp-empty">보고서 목록을 불러올 수 없음: ${esc(e.message)}</p>`; return; }
    renderReportList();
  }
  function renderReportList() {
    H("#rp-list").innerHTML = rp.list.length ? rp.list.map((r) => {
      const hl = Object.entries(r.headline || {}).slice(0, 4).map(([k, v]) => `<span>${esc(HEADLINE_KO[k] || k)} <b>${esc(v)}</b></span>`).join("");
      return `<div class="rp-item${rp.sel === r.id ? " on" : ""}" data-rp="${esc(r.id)}"><div class="rp-t"><b>${esc(r.title || r.id)}</b><span class="muted small">${esc(PERIOD_KO[r.period] || r.period || "")}</span></div>
        <div class="muted small">${esc(rpRange(r))}</div>${hl ? `<div class="rp-hl">${hl}</div>` : ""}
        <div class="rp-act"><button data-rp-open="${esc(r.id)}">열기</button>${r.hasPdf !== false ? `<a href="${esc(reportUrl(r.id, true))}" download>PDF</a>` : ""}</div></div>`;
    }).join("") : `<p class="muted small rp-empty">아직 보고서가 없습니다. '지금 만들기'로 만들 수 있습니다.</p>`;
  }
  function showReport(id) {
    rp.sel = id;
    $("#rp-frame").src = reportUrl(id);
    $("#rp-frame").hidden = false; $("#rp-empty").hidden = true;
    renderReportList();
  }
  function openReports() {
    if (ov.open) closeChat();
    $("#reports").hidden = false; document.body.classList.add("rp-lock");
    // Back button closes it (one history entry while open).
    if (!window.history.state?.rp) window.history.pushState({ ...(window.history.state || {}), rp: 1 }, "");
    $("#rp-frame").hidden = !rp.sel; $("#rp-empty").hidden = !!rp.sel;
    loadReports();
  }
  const closeReports = (fromPop) => {
    if ($("#reports").hidden) return;
    $("#reports").hidden = true; document.body.classList.remove("rp-lock");
    if (fromPop !== true && window.history.state?.rp) window.history.back();
  };
  window.addEventListener("popstate", () => closeReports(true));
  // "11/2–11/8 · 생성 11/9 12:30": from/to may be ms (to = exclusive midnight) or "YYYY-MM-DD" (inclusive).
  function rpRange(r) {
    const day = (v, end) => { if (v == null || v === "") return ""; let d = typeof v === "number" || /^\d{10,}$/.test(String(v)) ? new Date(+v) : new Date(`${v}T00:00:00`);
      if (Number.isNaN(+d)) return String(v); if (end && typeof v !== "string" || end && /^\d+$/.test(String(v))) { if (d.getHours() === 0 && d.getMinutes() === 0) d = new Date(+d - 1); } return `${d.getMonth() + 1}/${d.getDate()}`; };
    const a = day(r.from), b = day(r.to, true), g = r.generatedAt ? new Date(+r.generatedAt) : null;
    const gen = g && !Number.isNaN(+g) ? `${L2("생성", "made")} ${g.getMonth() + 1}/${g.getDate()} ${String(g.getHours()).padStart(2, "0")}:${String(g.getMinutes()).padStart(2, "0")}` : "";
    return [a || b ? (a === b ? a : `${a}–${b}`) : "", gen].filter(Boolean).join(" · ");
  }
  async function makeReport() {
    const period = $("#rp-period").value;
    const body = { period };
    if (period === "custom") {
      body.from = $("#rp-from").value; body.to = $("#rp-to").value;
      if (!body.from || !body.to) return toast("기간을 고르세요");
    }
    const btn = $("#rp-make");
    btn.disabled = true; btn.textContent = "만드는 중…";
    const done = () => { clearInterval(rp.timer); btn.disabled = false; btn.textContent = "지금 만들기"; };
    try {
      const r = await api("POST", "/api/reports", body);
      toast("보고서를 만들고 있습니다", 2500);
      // Generation runs in the background: poll the list until the new report appears.
      const want = r?.id;
      let tries = 0;
      clearInterval(rp.timer);
      rp.timer = setInterval(async () => {
        await loadReports();
        const hit = want ? rp.list.find((x) => x.id === want) : null;
        if (hit || ++tries > 40) { done(); if (hit) showReport(hit.id); }
      }, FIXTURE ? 300 : 3000);
    } catch { done(); }
  }
  $("#open-reports").addEventListener("click", openReports);
  $("#rp-close").addEventListener("click", () => closeReports());
  $("#rp-make").addEventListener("click", makeReport);
  $("#rp-period").addEventListener("change", (e) => { const c = e.target.value === "custom"; $("#rp-from").hidden = !c; $("#rp-to").hidden = !c; });
  $("#reports").addEventListener("click", (e) => {
    if (e.target.closest("a")) return;
    const o = e.target.closest("[data-rp-open]") || e.target.closest(".rp-item");
    if (o) return showReport(o.dataset.rpOpen || o.dataset.rp);
    if (e.target === $("#reports")) closeReports();
  });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && !$("#reports").hidden && !$("#dlg").open) { e.preventDefault(); e.stopImmediatePropagation(); closeReports(); } }, true);

  if (FIXTURE) $("#fixture-badge").hidden = false;
  // ---------- demo simulation (fixture / `adash demo`): client-side only, no /api ----------
  // Every 5 s one step of a loop: an agent starts work, another finishes and waits on you, a question appears,
  // a review request goes CRITIC → EDITOR and its answer is relayed back, an agent hits a rate limit and recovers.
  // ?sim=0 turns it off (static screenshots); ?simms=N sets the step interval.
  if (FIXTURE && new URLSearchParams(location.search).get("sim") !== "0") {
    const SIM_T = "T-demo-sim";
    const st = (n) => fixtureData?.agents.find((a) => a.name === n);
    const hs = (n) => fixtureHist?.agents.find((a) => a.name === n);
    const t0 = () => nowTick();
    function simSet(n, state, extra = {}) {
      const a = st(n), h = hs(n), now = t0();
      if (a) Object.assign(a, { state, stateSince: now, needsHuman: ["waiting", "done", "asking", "error"].includes(state) }, extra);
      if (h) { const last = h.segments[h.segments.length - 1]; if (last) last.end = now; h.segments.push({ state: state === "done" ? "waiting" : state, start: now, end: now, src: "recorded" }); }
    }
    function tickAll() {
      const now = t0();
      if (fixtureHist) { fixtureHist.now = now; for (const h of fixtureHist.agents) { const l = h.segments[h.segments.length - 1]; if (l && h.live !== false && l.end >= now - 120000) l.end = now; } }
      if (fixtureData) fixtureData.generatedAt = now;
    }
    const thread = () => fixtureData.threads.find((t) => t.id === SIM_T);
    const hthread = () => fixtureHist.threads.find((t) => t.id === SIM_T);
    const STEPS = [
      () => simSet("MODELER", "working", { tool: "bash", lastMessage: "", prompt: "Fit the regional model" }),
      () => simSet("FLEET", "done", { tool: null, lastMessage: "Rebalanced 12 jobs across 4 GPUs.\nQueue is empty." }),
      () => simSet("MODELER", "asking", { tool: "ask", ask: { question: "Which learning-rate schedule should I use?", options: [{ label: "cosine", description: "same as the last run" }, { label: "linear warmup", description: "better for short runs" }], multi: false, askedAt: t0() } }),
      () => {
        const now = t0();
        fixtureData.threads.push({ id: SIM_T, from: "CRITIC", to: "EDITOR", status: "queued", hops: 1, createdAt: now, request: "Please check that the numbers in §3 match Table 2.", replyPreview: null, dir: `~/.local/share/adash/threads/${SIM_T}` });
        fixtureHist.threads.push({ id: SIM_T, from: "CRITIC", to: "EDITOR", createdAt: now, deliveredAt: now, answeredAt: null, relayedAt: null, status: "queued" });
        simSet("EDITOR", "working", { tool: "read" });
      },
      () => { const now = t0(); Object.assign(thread(), { status: "answered", replyPreview: "Two differ: §3.2 0.91 → 0.94" }); Object.assign(hthread(), { status: "answered", answeredAt: now }); simSet("EDITOR", "waiting", { tool: null }); },
      () => { const now = t0(); Object.assign(thread(), { status: "relayed" }); Object.assign(hthread(), { status: "relayed", relayedAt: now }); simSet("CRITIC", "working", { tool: "edit" }); },
      () => simSet("WRITER", "error", { tool: null, lastError: { status: 429, message: "rate_limit_error: Number of request tokens has exceeded your per-minute rate limit", provider: "anthropic", model: "claude-opus-5-5", at: t0() } }),
      () => { simSet("WRITER", "working", { tool: "read" }); delete st("WRITER").lastError; },
      () => {
        simSet("MODELER", "idle", { tool: null, ask: null }); simSet("FLEET", "working", { tool: "bash" }); simSet("CRITIC", "waiting", { tool: null });
        fixtureData.threads = fixtureData.threads.filter((t) => t.id !== SIM_T); fixtureHist.threads = fixtureHist.threads.filter((t) => t.id !== SIM_T);
      },
    ];
    let step = 0;
    setInterval(async () => {
      if (!fixtureData || !fixtureHist) return;
      tickAll();
      try { STEPS[step % STEPS.length](); } catch (e) { console.warn("demo step", step, e); }
      step++;
      await refresh(); await refreshHistory();
    }, Math.max(1000, +new URLSearchParams(location.search).get("simms") || 5000));
  }
  startUpdates();
})();
