// adash UI language (v21): Korean is the source language; `?lang=en` (saved per device) switches the UI chrome to
// English. One dictionary of Korean UI phrases → English, applied to UI text nodes and to title / placeholder /
// aria-label / data-tip attributes as they appear (MutationObserver), so every renderer stays Korean-first.
// Agent-authored content (message bodies, questions, skill descriptions, names) is never translated: see SKIP.
(() => {
  const qs = new URLSearchParams(location.search);
  const want = qs.get("lang");
  if (want === "en" || want === "ko") localStorage.setItem("adash.lang", want);
  const LANG = localStorage.getItem("adash.lang") === "en" ? "en" : "ko";
  window.ADASH_LANG = LANG;
  document.documentElement.lang = LANG;
  window.t = (s) => (LANG === "en" ? translate(String(s)) : String(s));
  // Content marker for tooltips: in English the line remainder is left untranslated; in Korean it's a no-op.
  window.raw = (s) => (LANG === "en" ? `\u2063${s ?? ""}\u2064` : String(s ?? ""));
  if (LANG !== "en") return;
  // Server-written labels follow the UI language: every /api request carries lang=en (agent text stays as is).
  const ofetch = window.fetch.bind(window);
  window.fetch = (input, init) => {
    if (typeof input === "string" && /^\/api\//.test(input) && !/[?&]lang=/.test(input)) input += (input.includes("?") ? "&" : "?") + "lang=en";
    return ofetch(input, init);
  };
  const OES = window.EventSource;
  if (OES) window.EventSource = function (url, cfg) { if (typeof url === "string" && /^\/api\//.test(url) && !/[?&]lang=/.test(url)) url += (url.includes("?") ? "&" : "?") + "lang=en"; return new OES(url, cfg); };

  // Dates: ko-KR formatting becomes en-US (times, "10/28, 9:51 AM").
  for (const fn of ["toLocaleString", "toLocaleTimeString", "toLocaleDateString"]) {
    const orig = Date.prototype[fn];
    Date.prototype[fn] = function (loc, opts) { return orig.call(this, loc === "ko-KR" ? "en-US" : loc, opts); };
  }

  const UNITS = [
    [/컴팩션 (\d+)회/g, (_, n) => (n === "1" ? "1 compaction" : `${n} compactions`)], [/ 순서(?= · |$)/g, " order"], [/로그 축척 (\d+)개 패널/g, "log scale in $1 panels"],
    [/^(\d+)시$/g, "$1:00"], [/캐시가 식기까지 /g, "Cache cools in "], [/초기화 (.+?) 후/g, "resets in $1"], [/오류로 멈춘 에이전트 (\d+)개를 모두 다시 시도/g, "Retry all $1 agents stopped on errors"], [/도구 (\d+)개/g, "$1 tools"], [/(\d+(?:\.\d+)?)\s?시간 전부터/g, "for $1 h"], [/(\d+(?:\.\d+)?)\s?일 전부터/g, "for $1 d"], [/(\d+)\s?분 전부터/g, "for $1 min"],
    [/(\d+(?:\.\d+)?)\s?시간 전에? 답함/g, "answered $1 h ago"], [/(\d+(?:\.\d+)?)\s?일 전에? 답함/g, "answered $1 d ago"], [/(\d+)\s?분 전에? 답함/g, "answered $1 min ago"],
    [/(\d+(?:\.\d+)?)\s?시간 전 답/g, "answered $1h ago"], [/(\d+(?:\.\d+)?)\s?일 전 답/g, "answered $1d ago"], [/(\d+)\s?분 전 답/g, "answered $1m ago"],
    [/(\d+(?:\.\d+)?)\s?시간 전/g, "$1 h ago"], [/(\d+(?:\.\d+)?)\s?일 전/g, "$1 d ago"], [/(\d+)\s?분 전/g, "$1 min ago"], [/(\d+)\s?초 전/g, "$1 s ago"],
    [/(\d+)\s?분째/g, "for $1 min"], [/(\d+(?:\.\d+)?)\s?시간째/g, "for $1 h"],
    [/(\d+)\s?분 미룸/g, "snoozed $1 min"], [/미루기 (\d+)분/g, "Snooze $1 min"],
    [/(\d+(?:\.\d+)?)\s?시간/g, "$1 h"], [/(\d+(?:\.\d+)?)\s?일(?![가-힣])/g, "$1 d"], [/(\d+)\s?분(?![가-힣])/g, "$1 min"], [/(\d+)\s?초(?![가-힣])/g, "$1 s"],
    [/(\d+)\s?개 선택/g, "$1 selected"], [/(\d+)\s?개 숨김/g, "$1 hidden"], [/(\d+)\s?개 확인함/g, "$1 marked read"], [/(\d+)\s?개 모두 재시도/g, "retry all $1"],
    [/(\d+)\s?개 에이전트에게 메시지/g, "Message to $1 agents"],
    [/(\d+)\s?개(?![가-힣])/g, "$1"], [/(\d+)\s?줄/g, "$1 lines"], [/(\d+)\s?장(?![가-힣])/g, "$1"], [/홉 (\d+|\?)/g, "hops $1"],
    [/(\d+)\/(\d+)\s?답함/g, "$1/$2 answered"], [/(\d+)\/(\d+)에서 멈춤/g, "stopped at $1/$2"],
  ];
  const P = {
    // ----- states / counts / top bar -----
    "내 차례": "My turn", "질문": "Questions", "미확인": "Unread", "기타": "Other", "작업 중": "Working", "오류": "Error", "곧 식음": "Cooling soon",
    "보류": "Held", "초안": "Draft", "결정": "Decide", "유휴": "Idle", "오프라인": "Offline", "미룸": "Snoozed", "대기": "Waiting", "완료": "Done", "막힘": "Blocked",
    "질문 대기": "Asking", "승인 대기": "Awaiting approval", "오류로 멈춤": "Stopped on error", "답변 보류": "Reply held", "미확인 답변": "Unread reply",
    "기타 대기": "Other waits", "그 밖의 대기": "Other waits", "누적 대기": "Total wait", "캐시 식음": "Cache cold", "캐시 따뜻": "Cache warm", "캐시 식는 중": "Cache cooling", "캐시 차가움": "Cache cold",
    "오류 모두 재시도": "Retry all errors", "작업 중 ·": "Working ·", "전 답": "ago", "답": "reply",
    "내 차례 = 질문": "My turn = questions", "오류 = 제공자 API 오류로 멈춘 에이전트": "Error = agents stopped on a provider API error",
    "기타 대기 = 권한·승인 대기, 답변 보류": "Other waits = permission/approval waits, held replies", "입력창 초안으로 막힌 전달 · 미룬 것 제외": "deliveries blocked by an input draft · snoozed excluded",
    "기록 없음: 현재 상태만": "No history: current state only", "갱신": "Updated", "연결 실패:": "Connection failed:",
    // ----- top bar / menus -----
    "새 세션": "New session", "보기 ▾": "View ▾", "필터": "Filter", "정렬": "Sort", "전체": "All", "대기만": "Waiting only", "라이브만": "Live only",
    "지난 세션": "Past sessions", "지난 세션을 찾아 보거나 이어서 엶": "Find a past session to view or resume", "제목·이름·프로젝트·첫 지시로 찾기": "Filter by title, name, project or first prompt",
    "보기": "View", "이어서 열기": "Resume", "채팅 열기": "Open chat", "← 목록": "← List", "지난 세션 목록으로": "Back to past sessions", "일치하는 세션 없음": "No matching session", "찾아서 이어서 열기": "find and resume",
    "최근 상호작용순": "Recent interaction", "빠뜨린 것 먼저": "Missed first", "연결 보기": "Links", "기본": "Basic", "모두": "All", "끔": "Off", "검색": "Search", "더보기": "More",
    "검토 요청 목록": "Review requests", "에이전트끼리 주고받은 검토·제안 요청과 진행 상태": "Review/propose requests between agents and their status",
    "내 할 일": "My tasks", "나만 할 수 있는 일 (전화, 서류 제출 등) · 에이전트 작업 아님": "Things only I can do (calls, paperwork) · not agent work",
    "프로젝트 설정": "Project settings", "프로젝트별 목표·마감·우선순위 점수 (impact·leverage·cost)": "Per-project goal, deadline, priority score (impact·leverage·cost)",
    "보고서": "Reports", "주간·월간 사용 보고서 보기·만들기": "View or create weekly/monthly usage reports", "이름 묻기": "Ask names",
    "이름이 모호한 에이전트들에게 이름을 물어봄": "Ask agents with ambiguous names to name themselves", "표시 설정": "Display settings", "글꼴·글자 크기·곧 식음 기준": "Font, text size, cooling threshold",
    "필터·정렬·연결 보기": "Filter, sort, links", "에이전트·프로젝트로 이동, ?로 대화 검색": "Jump to agent/project; ? searches chats", "에이전트": "Agents", "현재": "Now", "지금": "now",
    "오프라인 접기": "Hide offline", "종료된 에이전트 (같은 이름의 이전 세션 포함) · 펼치면 표시": "Ended agents (incl. earlier same-name sessions) · expand to show", "병목": "Bottleneck",
    "요청 구간 띠 + 짧은 연결 눈금": "Request bands + short connector ticks", "모든 요청·회신 곡선": "All request/reply curves", "용량": "Capacity", "용량…": "Capacity…", "용량 ?": "Capacity ?",
    "전 기준": "ago", "누르면 새로고침": "click to refresh", "오래됨": "stale",
    // ----- timeline tooltips -----
    "나": "me", "릴레이": "relay", "시스템": "system", "복원": "reconstructed", "회신": "reply", "요청": "request", "검토": "Review", "제안": "Propose", "검토·제안": "Review·Propose",
    "(자동 감지된 교환)": "(auto-detected exchange)", "자동 감지된 교환": "auto-detected exchange", "의 답이": "'s reply reached", "에게 전달됨": "",
    "마지막 메시지": "Last message", "내가 보낸 말": "My message", "정하실 것": "To decide", "진행": "Progress", "터미널 입력창 초안": "Terminal input draft",
    "질문 대기": "Asking", "여러 개 선택": "multiple choice", "내 마지막 상호작용": "my last interaction", "마감": "deadline", "목표:": "goal:", "목표": "Goal",
    // ----- status chips -----
    "오류 ·": "Error ·", "실패하거나 에스컬레이션된 검토 요청이 있음": "A review request failed or was escalated", "답변 보류 ·": "Reply held ·", "의 검토 답변 ·": "'s review reply ·",
    "재시도": "Retry", "재시도 중…": "Retrying…", "F5 재시도": "F5 retry", "같은 요청을 다시 보냅니다. 결과는 토스트로 알려 줍니다.": "Re-sends the same request; the result appears as a toast.",
    "제공자 API 오류로 멈춤": "Stopped on a provider API error", "제공자 API 오류로 멈췄습니다.": "Stopped on a provider API error.", "멈춤": "stopped",
    "재개됨": "resumed", "아직 오류": "still failing", "개 · 클릭하면 채팅 열기": " · click to open the chat",
    "네트워크(VPN·프록시·회선)를 바꿨나요? 그대로라면 같은 오류가 다시 날 수 있습니다.": "Did you change the network (VPN, proxy, line)? If not, the same error may happen again.",
    "모두 재시도": "Retry all", "오류로 멈춘 에이전트": "agents stopped on errors", "를 모두 다시 시도": "— retry all",
    // ----- recent activity -----
    "최근 내 활동": "My recent activity", "에이전트 간": "Agent-to-agent", "내가 보낸 말 최근": "My last", "이 기간에 보낸 말 없음": "Nothing sent in this window",
    "상호작용 기록 없음 (백엔드 대기)": "No interaction history (backend pending)", "답변 있음": "Answered", "이어서 대화": "Continued", "응답 없음": "No reply", "종료됨": "Ended",
    "답함": "answered", "이후": "then", "에 내가 답장": " — I replied", "에 내가 다시 보냄": " — I wrote again", "질문에 답함": "Answered a question", "로그인 필요": "Sign-in needed", "터미널 보기": "Terminal view", "브라우저에서 열기": "Open in browser", "장치 코드": "Device code", "복사": "Copy", "받은 코드를 붙여 넣고 Enter (터미널로 보냄)": "Paste the code and press Enter (sent to the terminal)", "(터미널 화면 없음)": "(no terminal screen)", "터미널 화면을 불러올 수 없음:": "Could not load the terminal screen:", "터미널에 보내지 못함:": "Could not send to the terminal:", "터미널에 보낼 글 · Enter 로 보냄": "Text for the terminal · Enter sends", "도구가 보여 준 이미지": "Image from a tool", "직접:": "typed:",
    "에이전트끼리 보낸 메시지 (검토·제안 요청, 회신, adash tell) · 클릭하면 받는 쪽 채팅 열기": "Messages between agents (review/propose requests, replies, adash tell) · click to open the receiver's chat",
    "이 기간에 에이전트 간 메시지 없음": "No agent-to-agent messages in this window", "메시지": "Message", "회신 전달": "Reply relayed", "답 없음": "no answer",
    "에이전트 간 메시지 기록 없음 (백엔드가 human/agent 를 나눠 보내면 여기에 표시)": "No agent-to-agent history yet (shown once the backend splits human/agent)",
    // ----- chat overlay -----
    "터미널 열기": "Open terminal", "이 에이전트의 tmux 터미널 열기": "Open this agent's tmux terminal", "검토 ▾": "Review ▾", "검토 요청": "Request review",
    "검토 역할 에이전트에게 적대적 검토를 맡김 (magi)": "Ask the critic agent for an adversarial review (magi)", "직접 검토 요청…": "Custom review request…",
    "대상 에이전트와 요청 내용을 직접 정해서 보냄": "Pick the target agent and write the request yourself", "제안 역할 에이전트에게 선택지·시안 제안을 맡김 (magi)": "Ask the proposer agent for options/drafts (magi)",
    "미루기 ▾": "Snooze ▾", "15분": "15 min", "1시간": "1 h", "오늘 끝까지": "Rest of today", "미루기 해제": "Unsnooze", "미룸": "snoozed",
    "이름 ▾": "Name ▾", "바꾸기": "Rename", "대시보드에서 쓸 이름을 직접 정함": "Set the name the dashboard uses", "묻기": "Ask", "에이전트에게 스스로 이름을 정해 달라고 요청": "Ask the agent to name itself",
    "이전·다음 내 차례 · e 확인함 · r 입력 · Esc 닫기": "[ ] prev/next · e mark read · r reply · Esc close", "닫기 (Esc)": "Close (Esc)", "표시 설정 (글꼴·글자 크기)": "Display settings (font, size)",
    "대화": "Chat", "터미널": "Terminal", "대시보드": "Dashboard", "정보": "Info", "검토 요청 ": "Reviews ", "에이전트끼리 주고받은 검토·제안 요청": "Review/propose requests between agents", "나만 할 수 있는 일": "Things only I can do",
    "프로젝트별 목표·마감·우선순위 점수": "Per-project goal, deadline, priority", "이름": "Name", "역할": "Role", "프로젝트": "Project", "제목": "Title", "종류": "Type", "상태": "State", "캐시": "Cache",
    "도구": "Tool", "대화 출처": "Transcript source", "경로": "Path", "정확": "exact", "추정": "estimated", "불러올 수 없음": "unavailable", "선택된 에이전트 없음": "No agent selected",
    "에이전트가 더 이상 없습니다.": "agent no longer exists.", "보드에서 에이전트를 고르면 대화가 여기에 열립니다.": "Pick an agent on the board to open its chat here.",
    "대화 기록을 불러올 수 없어 마지막 메시지만 보여 줍니다.": "Transcript unavailable; showing the last message only.", "메시지 없음": "No messages", "불러오는 중…": "Loading…",
    "새 메시지 ↓": "New messages ↓", "첨부": "Attach", "이미지 첨부 (붙여넣기·끌어다 놓기도 가능)": "Attach images (paste or drop also works)", "보내기": "Send", "보내는 중…": "Sending…",
    "메시지 입력 · Enter 보내기 · Shift+Enter 줄바꿈 · Ctrl+Enter 강제 전송 · 이미지 붙여넣기/끌어다 놓기": "Message · Enter send · Shift+Enter newline · Ctrl+Enter force · paste/drop images",
    "메시지 입력 · Enter 줄바꿈 · 보내기는 버튼으로 · 이미지 붙여넣기": "Message · Enter newline · send with the button · paste images", "메시지 입력 · Enter 줄바꿈 · 보내기는 버튼으로 · 파일 붙여넣기": "Message · Enter newline · send with the button · paste files",
    "전달됨": "Delivered", "전달되지 않음": "Not delivered", "한 줄": "line", "붙여넣기": "paste", "파일": "file", "이미지": "image",
    "에이전트가 작업 중입니다": "The agent is working", "아직 보내지 않았습니다.": "Not sent yet.", "그래도 전송": "Send anyway", "취소": "Cancel",
    "입력창에 초안이 있을 수 있습니다": "There may be a draft in its input", "작업 중 ·": "Working ·", "중단": "Interrupt", "중단 요청 보냄": "Interrupt sent",
    "진행 중인 작업을 중단(인터럽트)합니다. 계속할까요?": "This interrupts the current work. Continue?", "이어서 진행": "Continue", "계속 진행해": "Please continue",
    "입력창에 '계속 진행해'를 넣습니다. 보내기는 직접 누르세요.": "Puts 'Please continue' in the input; you press Send.",
    "내가 실행한 셸 명령": "Shell command I ran", "출력": "Output",
    // ----- decisions / drafts / magi / relays -----
    "모두 답하기": "Answer all", "항목이나 선택지를 누르면 입력창에 번호와 함께 채워집니다": "Click an item or option to fill it into the input with its number",
    "터미널 입력창에 보내지 않은 초안:": "Unsent draft in the terminal input:", "입력창에 보내지 않은 초안": "Unsent draft in the input", "이어서 보내기": "Submit it", "지우기": "Clear",
    "내 입력창으로 가져오기": "Move to my input", "초안 지우기": "Clear draft", "입력창에 남아 있는 초안을 지웁니다. 되돌릴 수 없습니다.": "Clears the draft left in the input. This cannot be undone.",
    "입력창 초안도 지울까요?": "Also clear the terminal input draft?", "내용은 대시보드 입력창으로 옮겼습니다. 터미널 쪽 초안을 지우지 않으면 다음 전송 때 함께 제출될 수 있습니다.": "The text moved to the dashboard input. If the terminal draft stays, it may be submitted with the next send.",
    "초안을 보냈습니다": "Draft submitted", "초안을 지웠습니다": "Draft cleared",
    "magi 검토": "magi review", "magi 제안": "magi propose", "magi 검토·제안": "magi review·propose", "전달 대기": "Queued", "진행 중": "Running", "답 모두 도착": "All answered",
    "종합 지시함": "Synthesis sent", "실패": "Failed", "취소됨": "Cancelled", "아직 안 물음": "Not asked yet", "때문에 보류": "held because of", "그래도 전달": "Deliver anyway",
    "입력창 초안이 있어도 전달 (작업 중인 턴에는 넣지 않음)": "Deliver even over an input draft (never into a working turn)", "magi 요청 취소": "Cancel magi request",
    "요청을 취소합니다. 이미 대상에게 간 질문은 되돌리지 않습니다.": "Cancels the request. Questions already sent to targets stay sent.", "취소하기": "Cancel request",
    "magi 요청을 취소했습니다": "magi request cancelled", "전달했습니다": "Delivered", "아직 보류:": "Still held:", "이미 전달된 요청입니다": "Already delivered", "magi 요청": "magi requests",
    "입력창 초안": "input draft", "입력창 확인 불가": "input unreadable", "답변 보류": "Reply held", "RS의 검토 답변이 도착했지만 입력창 초안 때문에 전달되지 않았습니다.": "RS's review reply arrived but was not delivered because of an input draft.",
    "의 검토 답변이 도착했지만": "'s review reply arrived but", "때문에 전달되지 않았습니다.": "was not delivered because of it.", "답변 열기": "Open reply", "초안과 함께 전달": "Deliver with draft",
    "초안과 함께 전달": "Deliver with draft", "의 검토 답변 안내 한 줄을 지금 보냅니다. 요청한 에이전트의 입력창에 남아 있는 초안이 이 줄과 함께 제출됩니다. 계속할까요?": "'s review-reply pointer is sent now. The draft left in the requester's input is submitted with it. Continue?",
    "전달 요청 보냄": "Delivery requested", "스레드 폴더를 알 수 없습니다": "Thread folder unknown",
    // ----- ask card -----
    "질문": "Question", "권한 요청": "Permission request", "부터": "since", "로그": "log", "추천": "recommended", "직접 답하기 (기타)": "Type an answer (other)", "답하기": "Answer",
    "답함": "answered", "모든 질문에 답하세요": "Answer every question", "이 질문은 대시보드에서 답할 수 없습니다. 터미널을 열어 답하세요.": "This question can't be answered from the dashboard. Open the terminal to answer it.",
    "선택지를 읽지 못했습니다. 선택지가 있는 질문이면": "Couldn't read the options. If it has options,", "로 확인해 답하세요. 직접 입력으로 답할 수도 있습니다.": "and answer there. You can also type an answer.",
    "답 넣는 중": "Entering answers", "끝날 때까지 다른 조작은 잠시 막아 둡니다": "Other actions are paused until it finishes", "창 여는 중": "opening", "선택하는 중": "selecting",
    "입력하는 중": "typing", "제출하는 중": "submitting", "반영 확인 중": "verifying", "답을 넣었습니다": "Answers entered", "반영은 확인하지 못함": "not verified",
    "터미널에서 한 번 확인해 주세요.": "Please check in the terminal.", "진행 상태를 확인하지 못했습니다": "Couldn't check progress",
    "답이 들어갔을 수 있습니다. 다시 보내기 전에 터미널에서 확인하세요.": "The answer may have gone in. Check the terminal before resending.", "답을 넣지 못했습니다": "Couldn't enter the answers",
    "이유 없음": "no reason given", "다시 시도": "Retry", "답을 보내지 못했습니다:": "Couldn't send the answer:", "답을 넣는 중입니다. 끝난 뒤 보내세요": "Entering answers; send after it finishes",
    "누르면 크게 보기": "click to enlarge", "비교 이미지:": "comparison image:",
    // ----- slash menu / prefixes / skills -----
    "스킬": "Skills", "명령": "Commands", "최근": "Recent", "프로젝트": "Project", "사용자": "User", "내장": "Built-in", "와 일치하는 스킬·명령 없음": " — no matching skill or command",
    "이 에이전트의 스킬 목록을 불러오지 못함 (": "Couldn't load this agent's skills (", "누르면 입력창에 넣기": "click to insert into the input",
    "로 시작하면 omp 가 셸 명령으로 실행합니다": " at the start makes omp run it as a shell command", "텍스트로 보내기": "Send as text", "셸 명령으로 실행": "Run as shell command",
    "으로 실행": " — run", "특수 명령": "special command", "셸 명령": "shell command", "메모리 추가": "memory add", "로 시작하면": " at the start:", "으로 처리됩니다": "",
    // ----- dialogs / launch / tasks / meta / reports -----
    "새 세션:": "New session:", "기존 프로젝트": "Existing project", "새 worktree": "New worktree", "새 프로젝트 폴더": "New project folder", "프로젝트·worktree 검색": "Search projects/worktrees",
    "저장소": "Repository", "기준 브랜치": "Base branch", "(선택) 비우면 기본값": "(optional) default if empty", "폴더 만들기": "Create folder", "모델": "Model", "첫 지시": "First instruction",
    "(선택)": "(optional)", "(선택) 여러 줄 가능": "(optional) multiple lines", "시작": "Start", "닫기": "Close", "일치하는 프로젝트 없음": "No matching project",
    "대상 프로젝트를 고르세요": "Pick a target project", "저장소와 이름을 입력하세요": "Enter repository and name", "경로를 입력하세요": "Enter a path", "여는 중…": "Opening…",
    "할 일": "Task", "추가": "Add", "프로젝트 설정 저장": "Save project settings", "점수 =": "score =", "메모": "Note", "메타 저장됨": "Settings saved",
    "모든 프로젝트가 기본 점수(impact·leverage·cost = 3)라서 보드의 ★ 점수를 숨겨 두었습니다. 한 프로젝트라도 점수를 바꾸면 ★가 다시 나타나고 '빠뜨린 것 먼저' 정렬에 반영됩니다.": "All projects have default scores (impact·leverage·cost = 3), so the ★ score is hidden. Change any project's score to show ★ and use it in 'Missed first'.",
    "보내는 쪽": "From", "받는 쪽": "To", "사람(나)": "Human (me)", "요청 내용": "Request", "이름 바꾸기": "Rename", "이름 요청 보냄": "Name request sent",
    "에게 스스로 이름을 정하라는 한 줄 요청을 보냅니다. 작업 중이거나 입력 중인 에이전트는 건너뜁니다.": ": send each a one-line request to name itself. Working or typing agents are skipped.",
    "이름이 모호한 에이전트": "Agents with ambiguous names", "모호한 이름 일괄 묻기": "Ask ambiguous names", "건너뜀": "skipped",
    "대상": "Targets", "다른 프로젝트": "Other projects", "검토자 후보": "critic candidate", "제안자 후보": "proposer candidate", "역할의 에이전트가 없습니다": "role has no agent",
    "모드": "Mode", "(선택) 한 줄 메모": "(optional) one-line note", "대상을 고르세요": "Pick targets", "역할 정보를 불러오지 못했습니다:": "Couldn't load roles:",
    "메시지 보내기": "Send message", "확인함": "Mark read", "선택 해제": "Clear selection", "보내기 결과": "Send results",
    "개는 자동으로 강제 전송하지 않습니다. 각 채팅에서 확인하세요.": " were not force-sent. Check each chat.",     "보고서 열기": "Open reports", "알림·기기 설정": "Notifications & devices", "확인함 모두": "Mark all read", "미확인 전부": "all unread",
    "주간": "Weekly", "월간": "Monthly", "기간 지정": "Custom range", "지금 만들기": "Create now", "만드는 중…": "Creating…", "열기": "Open", "왼쪽에서 보고서를 고르세요": "Pick a report on the left",
    "아직 보고서가 없습니다. '지금 만들기'로 만들 수 있습니다.": "No reports yet. Use 'Create now'.", "보고서를 만들고 있습니다": "Creating the report", "기간을 고르세요": "Pick a range",
    "보고서 목록을 불러올 수 없음:": "Couldn't load reports:", "보고서는 아직 없습니다": "No reports yet", "보낸 메시지": "Messages sent", "활동 시간": "Active hours", "턴": "Turns", "비용": "Cost", "에이전트 작업 시간": "Agent work hours", "기간": "Period",
    "글꼴": "Font", "시스템 기본": "System default", "글꼴 이름": "Font name", "코드 글꼴": "Code font", "시스템 고정폭": "System monospace", "채팅 글자": "Chat text", "보드 배율": "Board zoom",
    "초 전": "s before", "Enter 로 보내기": "Enter sends", "이 기기에만 저장 · 끄면 Enter 는 줄바꿈": "Saved on this device · off = Enter adds a newline",
    "채팅 화면에서 Ctrl + 휠 / Alt + = − 로도 조절": "In chat, Ctrl + wheel / Alt + = − also adjust", "기본값": "Defaults", "언어": "Language",
    // ----- palette / search -----
    "이동 · Enter 열기 · Tab 검색 전환 · Esc 닫기": "↑↓ move · Enter open · Tab search mode · Esc close", "검색어를 입력하세요 (전체 대화)": "Type to search all chats",
    "일치하는 항목 없음": "No matches", "검색 실패:": "Search failed:", "읽기 전용 ·": "read-only ·",
    // ----- mobile / push / devices -----
    "프로젝트": "Project", "활동": "Activity", "기다리는 에이전트가 없습니다": "No agents waiting", "미루기 60분": "Snooze 60 min", "알림·기기": "Notifications",
    "이 기기 승인 대기": "This device is waiting for approval", "데스크톱의 adash 대시보드에서 이 기기를 승인하세요.": "Approve this device in the adash dashboard on the desktop.",
    "새 기기 승인 요청": "New device approval request", "휴대폰에 보이는 코드:": "Code shown on the phone:", "승인": "Approve", "해제": "Revoke", "기기": "Devices",
    "모르는 기기라면 취소하세요. 나중에 '알림·기기 설정'에서 승인할 수 있습니다.": "Cancel if you don't know this device. You can approve it later in Notifications & devices.",
    "기기를 승인했습니다": "Device approved", "기기를 해제했습니다": "Device revoked", "이 기기에서 푸시 받기": "Receive push on this device", "이 기기 푸시 알림": "Push on this device",
    "푸시 알림을 켰습니다": "Push enabled", "푸시 알림을 껐습니다": "Push disabled", "푸시 설정 실패:": "Push setup failed:", "조용한 시간": "Quiet hours", "저장": "Save",
    "내가 직접 물은 질문에 답이 옴": "An answer to my question arrived", "내가 말 건 에이전트가 답을 남김": "An agent I wrote to replied", "데스크톱을 5분 이상 쓰지 않을 때만": "Only when the desktop is idle 5+ min",
    // ----- misc toasts -----
    "복사": "Copy", "복사됨": "Copied", "복사 실패": "Copy failed", "차트 숨기기": "Hide chart", "차트 보기": "Show chart", "소스 보기": "Show source", "행별 축척": "per-row scale",
    "열기:": "Opened:", "로드 실패": "Load failed", "불러오기 실패:": "Load failed:", "문서를 열 수 없음:": "Can't open document:", "이미지를 불러올 수 없음:": "Can't load image:",
    "대화를 열 수 없음:": "Can't open chat:", "이미지를 올리는 중입니다": "Uploading images", "이미지 올리는 중": "Uploading image", "이미지가 아님:": "Not an image:", "너무 큼:": "Too large:",
    "최대 20 MB)": "max 20 MB)", "파일 첨부 (붙여넣기·끌어다 놓기도 가능, 최대 100 MB)": "Attach files (paste or drop also work, max 100 MB)", "메시지 입력 · Enter 보내기 · Shift+Enter 줄바꿈 · Ctrl+Enter 강제 전송 · 파일 붙여넣기/끌어다 놓기": "Message · Enter send · Shift+Enter newline · Ctrl+Enter force send · paste or drop files", "올리는 중": "Uploading", "확인 중": "Checking", "로컬 파일": "local file", "빼기": "Remove", "하네스": "Harness", "프로젝트 설정 저장됨": "Project settings saved", "다시 시도": "Retry", "파일 올리는 중": "Uploading files", "파일을 올리는 중입니다": "Files are still uploading", "붙여넣은 이미지": "pasted-image", "붙여넣은 파일": "pasted-file", "확인 실패": "check failed", "없는 파일": "file not found", "폴더는 첨부할 수 없음": "folders cannot be attached", "첨부 이미지": "Attached image", "준비됨": "ready", "네트워크 오류": "Network error", "오류:": "Error:", "실패:": "Failed:", "남은 미확인 없음": "No unread left",
    "열림 ✓": "Opened ✓", "에서 열기": " — open", "로 이동": " — go", "스레드 없음": "No threads", "상태": "Status", "보낸→받는": "From→To", "요청 / 답변": "Request / reply",
    "보류(초안 의심)": "Held (possible draft)", "전달 미확인": "Delivery unconfirmed", "답변됨": "Answered", "회신 완료": "Relayed", "에스컬레이션": "Escalated", "대기열": "Queued",
    "패널": "Panel", "현재 상태": "current state", "일": "d",
    "에 새 세션": " — new session", "여러 개 선택 (Ctrl/Shift+클릭)": "Multi-select (Ctrl/Shift+click)", "사람": "human", "내 마지막 메시지": "my last message",
    "클릭하면 채팅 열기": "click to open the chat", "세션": "session", "기준": "(as of)", "이동": "move", "예: Pretendard": "e.g. Pretendard",
    "~/work/새프로젝트": "~/work/new-project", "이 기기에만 저장": "Saved on this device", "한국어": "Korean",
    "질문 대기": "Asking", "에서 열기": " — open", "btw 닫기": "Close btw", "후속 질문…": "Follow-up…", "btw 열림 · 일반 메시지는 닫은 뒤 보낼 수 있음": "btw open · close it before sending a normal message", "btw 를 닫았습니다": "btw closed", "btw 후속 질문 보냄": "btw follow-up sent", "btw 후속 질문 보류:": "btw follow-up held:", "이 서버는 btw 후속 질문·닫기를 지원하지 않습니다": "This server does not support btw follow-up/close", "btw 가 열려 있습니다. 'btw 닫기' 후 보내세요 (후속 질문은 btw 답 아래 입력줄)": "A btw is open. Close it first (follow-ups go in the row under the btw answer)", "종료…": "Close…", "강제 종료": "Force close", "다시 열기": "Reopen", "⋯ ▾": "⋯ ▾", "에이전트 종료·다시 열기": "Close or reopen the agent", "에이전트 메뉴": "Agent menu",
    "작업을 멈추고 하네스를 정상 종료한 뒤 터미널을 닫음 · 세션은 남음": "Stops work, exits the harness cleanly, then closes the terminal · session kept",
    "터미널을 바로 닫음 · 세션은 남음": "Closes the terminal right away · session kept", "저장된 세션을 이어서 새 터미널로 엶": "Resume the saved session in a new terminal",
    "종료": "Close", "주의": "Caution",
    "작업 중입니다. 진행 중인 작업을 중단한 뒤 종료합니다.": "It is working. The current work is interrupted first.",
    "답을 기다리는 질문이 있습니다. 답하지 않고 종료합니다.": "It has an open question. It closes without an answer.",
    "입력창에 보내지 않은 초안이 있습니다 (버려짐):": "Its input has an unsent draft (discarded):",
    "터미널을 바로 닫습니다(하네스가 정리할 시간 없음). 세션 기록은 남아 나중에 '다시 열기'로 이어 갈 수 있습니다.": "Closes the terminal immediately (no time for the harness to clean up). The session is kept and can be resumed with Reopen.",
    "작업 중이면 먼저 중단하고, 하네스의 종료 명령으로 끝낸 뒤 터미널을 닫습니다. 세션 기록은 남아 나중에 '다시 열기'로 이어 갈 수 있습니다.": "Interrupts any running work, exits through the harness's own command, then closes the terminal. The session is kept and can be resumed with Reopen.",
    "종료하는 중…": "closing…", "종료됨 · 세션은 남아 있음": "closed · session kept", "종료됨": "Closed", "종료하지 않음:": "Not closed:", "종료 실패:": "Close failed:",
    "다시 여는 중…": "reopening…", "다시 열림": "reopened", "다시 열지 않음: 이미 같은 세션을 쓰는 프로세스가 있습니다": "Not reopened: a process already holds this session", "다시 열기 실패:": "Reopen failed:",
    "미루기 15분": "Snooze 15 min", "미루기 1시간": "Snooze 1 h", "이 서버는 경로 자동완성을 지원하지 않습니다": "This server does not support path completion", "와 일치하는 파일·폴더 없음": ": no matching files or folders", "tmux 서버 응답 없음": "tmux server not responding", "adash가 다시 띄우는 중입니다. 그동안 보내기와 터미널 조작은 실패합니다": "adash is restarting it; sends and terminal actions fail meanwhile", "경과 ·": "elapsed ·", "1분 미만": "<1 min", "네트워크 대기": "Waiting for network", "네트워크 끊김 · VPN 확인": "Network down · check VPN", "일부 제공자 접속 불가": "Some providers unreachable", "차단됨 (지역 제한)": "Blocked (region)", "접속 불가": "Unreachable", "정상": "OK", "자동 전송은 네트워크 대기": "automatic sends wait for the network", "자동 전송은 복구될 때까지 대기 · 눌러서 자세히": "Automatic sends wait until it recovers · tap for details", "네트워크 상태": "Network status", "제공자": "Provider", "지연": "Latency", "확인 시각": "Checked at", "지금 다시 확인": "Check now", "다시 확인했습니다": "Checked again", "확인 실패:": "Check failed:", "정보 없음": "No data", "접속할 수 없는 제공자의 에이전트에는 자동 전송(답변 전달·magi·이름 요청·자동 재시도)을 보내지 않고 대기합니다.": "Agents on an unreachable provider get no automatic sends (reply relays, magi, name requests, auto retries) until it recovers.", "네트워크 복구 후 자동 재시도": "Auto retry after network recovery", "네트워크 오류로 멈춘 에이전트를 복구 뒤 한 번 재시도 · 서버 설정": "Retries agents stopped by network errors once after recovery · server setting", "네트워크 복구 후 자동 재시도 켬": "Auto retry after recovery on", "네트워크 복구 후 자동 재시도 끔": "Auto retry after recovery off", "저장 실패:": "Save failed:", "네트워크 확인": "Network check", "그래도 보내기": "Send anyway", "그래도 보낼까요?": "Send anyway?", "제공자에 접속할 수 없습니다": "The provider is unreachable", "에이전트 입력창에는 들어가지만 응답이 오류로 끝날 수 있습니다. VPN·네트워크를 먼저 확인하세요.": "It reaches the agent's input, but the reply may end in an error. Check the VPN / network first.", "보냈지만 제공자에 접속할 수 없는 상태입니다": "sent, but the provider is unreachable", "캐시 수명": "Cache lifetime", "남음": "left", "사용량": "Usage", "경과": "elapsed", "컨텍스트": "Context", "컴팩션": "Compactions", "이 세션": "This session", "입력 토큰": "Input", "출력 토큰": "Output", "캐시 읽기": "Cache read", "캐시 쓰기": "Cache write", "계정": "Account", "곧 초기화": "resets soon", "초기화까지": "resets in", "mermaid 오류:": "mermaid error:", "로그 축척 (값 범위 10배 초과)": "log scale (range over 10×)", "비율 열은 행끼리 같은 축척": "ratio columns share one scale", "첫 메시지를 보내면 대화가 여기에 표시됩니다": "The conversation appears here once you send the first message", "터미널 보기": "View terminal", "숨기기 (에이전트는 계속 작업)": "Hide (agents keep working)", "오늘": "Today", "계속": "Until shown", "모두 종료…": "Close all…", "보관…": "Archive…", "모두 종료하고 계속 숨김 · 지난 세션에서 복원": "Close all and hide for good · restore from past sessions", "보관됨": "archived", "계속 숨김": "hidden until shown", "숨긴 프로젝트": "Hidden projects", "보이기": "Show", "모두 다시 열기": "Reopen all", "보관된 프로젝트": "Archived projects", "모두 종료": "Close all", "보관": "Archive", "프로젝트 메뉴": "Project menu", "확인이 필요한 에이전트": "Agents that need your OK", "작업 중이거나 질문·초안이 있는 에이전트입니다. 체크를 풀면 종료하지 않고 다시 묻습니다.": "These are working, asking or holding a draft. Untick one and nothing is closed; you will be asked again.", "강제 종료 (터미널을 바로 닫음)": "Force close (close terminals right away)", "파일·세션·기록은 지우지 않습니다.": "No files, sessions or history are deleted.", "살아 있는 에이전트를 모두 종료합니다. 세션 기록은 남아 하나씩 또는 한꺼번에 다시 열 수 있습니다.": "Closes every live agent. Sessions are kept and can be reopened one by one or all at once.", "살아 있는 에이전트를 모두 종료하고 이 프로젝트를 계속 숨깁니다. 지난 세션의 '보관된 프로젝트'에서 복원하고, 그때 살아 있던 세션을 한 번에 다시 열 수 있습니다.": "Closes every live agent and hides this project for good. Restore it from 'Archived projects' in past sessions and reopen the sessions that were live at that time in one go.", "이 프로젝트에는 조정자 세션도 있습니다. 종료하면 진행 중인 조정 작업도 멈춥니다.": "This project includes the coordinator's session. Closing it stops coordination work in progress.", "에이전트는 계속 작업": "agents keep working", "다시 보임": "shown again", "숨김": "hidden", "에이전트는 닫힌 채로 남음": "agents stay closed", "다시 연 세션": "sessions reopened", "보관할 때 살아 있던 세션": "sessions live at archive time", "남김/실패": "kept/failed", "보관함": "archived", "대상 에이전트": "Agents affected", "모두 종료하고 숨김": "close all and hide", "숨기기·종료·보관": "hide · close · archive", "숨기기": "Hide", "모두 확인했습니다": "All caught up", "터미널 열기 실패:": "Could not open a terminal:", "터미널을 열었습니다": "terminal opened", "은 이미 열린 tmux 창에 보입니다": "is already shown in an attached tmux window", "터미널 프로그램을 열지 못했습니다": "No terminal program could be opened", "터미널에서 아래 명령을 실행하면 이 에이전트 화면에 붙습니다.": "Run this command in a terminal to attach to this agent.", "복사됨": "Copied", "터미널을 열지 못했습니다": "could not open a terminal", "기호 및 상태 범례 (?)": "Legend of marks and states (?)", "범례": "Legend", "타임라인 막대": "Timeline bars", "기호": "Marks", "질문 대기 · 내 답이 필요 (ASK)": "Question waiting · needs my answer (ASK)", "오류로 멈춤 · 다시 시도 필요 (ERR)": "Stopped on an error · needs a retry (ERR)", "답을 보냄 · 내 차례 (TURN)": "Answered · my turn (TURN)", "작업 중 · 지켜보기만": "Working · just watch", "유휴 · 이전 대기": "Idle · earlier waiting", "캐시 만료 예상까지 남은 시간": "Time until the cache is expected to expire", "캐시 만료 예상": "Cache expiry estimate", "정하실 것 (결정 항목 수)": "Decisions to make", "답변 보류 (입력창 초안 등)": "Reply held (e.g. a draft in the input)", "magi 검토 진행": "magi review progress", "터미널 입력창에 보내지 않은 초안": "Unsent draft in the terminal input", "작업 단계 진행률": "Task progress", "접힌 보조 표시 (마우스를 올리면 전부)": "Folded extra marks (hover for all)", "프로젝트 우선순위 점수": "Project priority score", "이름을 자동으로 추정함": "Name guessed automatically", "테마 (밝을 때)": "Theme (light)", "테마 (어두울 때)": "Theme (dark)", "적용": "Use", "시스템 밝기 따라": "Follow system", "항상 밝은 테마": "Always light", "항상 어두운 테마": "Always dark", "내 테마": "My themes", "로 시작": "to start", "저장하면 바로 반영": "changes apply on save", "(내 테마)": "(mine)", "adash 전용 tmux 서버에서 실행 중인 에이전트가 없습니다": "No agents are running on the adash tmux server", "새 세션을 시작하거나 지난 세션을 이어서 여세요. 둘러보기만 하려면": "Start a new session or resume a past one. To just look around, run", "새 세션 시작 (n)": "New session (n)", "지금 나를 기다리는 에이전트가 없습니다": "No agent is waiting on you", "전체 에이전트 보기": "Show all agents", "타임라인 기록을 불러오지 못해 현재 상태만 보여 줍니다": "Timeline history could not be loaded; showing the current state only", "기록 다시 불러오기": "Reload history", "마지막 화면을 읽기 전용으로 보여 주며 보내기는 잠시 보류됩니다": "showing the last snapshot read-only; sends are on hold", "백그라운드에서 계속 작업 (내 차례·알림에서 제외)": "keep working in the background (left out of My turn and notifications)", "오류가 있는 값은 기본 테마 값으로 대신합니다": "Values with errors fall back to the base theme", "(오류)": "(error)", "경고:": "warning:", "표시 중인 프로젝트가 없습니다": "No projects are shown", "숨긴 프로젝트의 에이전트는 계속 실행될 수 있습니다. 아래 '숨긴 프로젝트'에서 다시 보이게 할 수 있습니다.": "Agents in hidden projects may still be running. Show them again from 'Hidden projects' below.", "현재 필터에 맞는 에이전트가 없습니다": "No agents match the current filter", "질문·오류를 포함한 전체 상태는 '전체 에이전트 보기'에서 확인하세요.": "See every state, including questions and errors, with 'Show all agents'.", "나를 기다리는 에이전트": "agents waiting on you", "tmux 서버가 응답하지 않습니다": "The tmux server is not responding", "마지막 상태를 표시하며, 연결이 복구되기 전에는 보내기와 터미널 조작이 실패할 수 있습니다": "showing the last state; sends and terminal actions may fail until it recovers", "adash 서버에 연결할 수 없습니다": "Cannot reach the adash server", "마지막 수신": "last received", "마지막으로 받은 상태를 표시하고 있으며 현재 갱신되지 않습니다": "showing the last state received; it is not updating", "다시 연결": "Reconnect", "불러오지 않음:": "not loaded:", "아직 상태를 받지 못했습니다": "No state received yet", "adash 서버 연결을 확인하세요.": "Check the connection to the adash server.", "아직 상태를 받지 못했습니다. adash 서버 연결을 확인하세요": "No state received yet. Check the connection to the adash server", "tmux 서버가 응답하지 않아 에이전트 목록을 확인할 수 없습니다": "The tmux server is not responding, so the agent list cannot be checked", "연결이 복구되면 다시 조회합니다.": "It will be checked again once the connection recovers.", "도 같은 계열(": " too: same family (", ")로 맞췄습니다": ")", "어두울 때": "Dark mode", "밝을 때": "Light mode", "테마 찾기": "Find theme", "이름·계열 (예: gruvbox, dark)": "Name or family (e.g. gruvbox, dark)", "Okabe–Ito (색약 안전)": "Okabe–Ito (color-blind safe)", "Astra 띄우기": "Launch Astra", "btw 질문": "btw question", "btw 답": "btw answer", "답 기다리는 중": "waiting for the answer", "작성 중": "writing", "본 대화와 별개인 곁가지 질문 (omp btw)": "Side question outside the main thread (omp btw)", "Argon 띄우기": "Launch Argon", "제안자": "proposer", "검토자": "critic",
  };
  const KEYS = Object.keys(P).sort((a, b) => b.length - a.length);
  const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  // A phrase only matches as a whole Hangul run, so Korean words inside agent text ("진행해 줘") are left alone.
  const RE = new RegExp(`(?<![\\uAC00-\\uD7A3])(?:${KEYS.map(esc).join("|")})(?![\\uAC00-\\uD7A3])`, "g");
  const HANGUL = /[\uAC00-\uD7A3]/;
  // window.raw() wraps agent/user content (names, messages) in U+2063 … U+2064 (or to the end of the line):
  // those spans are kept verbatim, only the UI text around them is translated.
  function translate(s) {
    if (s.includes("\u2063")) return s.split(/(\u2063[^\u2064\n]*\u2064?)/).map((part) => (part.startsWith("\u2063") ? part : translate(part))).join("");
    if (!HANGUL.test(s)) return s;
    let out = s;
    for (const [re, to] of UNITS) out = out.replace(re, to);
    if (!HANGUL.test(out)) return out;
    return out.replace(RE, (m) => P[m]);
  }
  window.adashTranslate = translate;

  // Agent-authored content and proper names stay as they are.
  const SKIP = ".md, .said, .mc-first, .em-body, .aa-q, .aa-pick, .aa-own, .sl-d, .nm, .gname, .mp-n, .mc-head > b, .who > b, .who > .muted, .mc-meta > .muted, #ov-title > span, .hb, code, pre, kbd, .m.assistant:not(.btw), .m.system, .m.user:not(.shell):not(.ask-ans):not(.btw), .ro-m, .pal-s, .pal-l > b, .sl-n, .ld-list, .ld-item, .mg-tg, .rp-t > b, .ask-q .md, .ask-opt > b, .dc-text, .ds-text, .ri.ai .said, iframe, .pcand, .skchip, .pathlink, .relay-preview, #meta-editor input, #meta-editor textarea, #meta-editor h4, .task-t, .meta-proj h4, #threads td:nth-child(3), #humanTasks li > span:not(.muted), .dc-item, .dc-opt, .trow, .btw-err, [data-no-i18n]";
  const ATTRS = ["title", "placeholder", "aria-label", "data-tip", "alt"];
  const skip = (el) => !!el?.closest?.(SKIP);
  function fixNode(n) {
    if (n.nodeType === 3) {
      if (HANGUL.test(n.nodeValue) && !skip(n.parentElement)) { const v = translate(n.nodeValue); if (v !== n.nodeValue) n.nodeValue = v; }
      return;
    }
    if (n.nodeType !== 1) return;
    if (!skip(n)) for (const a of ATTRS) { const v = n.getAttribute(a); if (v && HANGUL.test(v)) { const w = translate(v); if (w !== v) n.setAttribute(a, w); } }
    if (skip(n)) return;
    const w = document.createTreeWalker(n, NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT, { acceptNode: (x) => (x.nodeType === 1 && skip(x) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT) });
    while (w.nextNode()) {
      const x = w.currentNode;
      if (x.nodeType === 3) { if (HANGUL.test(x.nodeValue)) { const v = translate(x.nodeValue); if (v !== x.nodeValue) x.nodeValue = v; } }
      else for (const a of ATTRS) { const v = x.getAttribute(a); if (v && HANGUL.test(v)) { const t2 = translate(v); if (t2 !== v) x.setAttribute(a, t2); } }
    }
  }
  let busy = false;
  const mo = new MutationObserver((recs) => {
    if (busy) return;
    busy = true;
    try {
      for (const r of recs) {
        if (r.type === "childList") r.addedNodes.forEach(fixNode);
        else if (r.type === "characterData") fixNode(r.target);
        else if (r.type === "attributes") fixNode(r.target);
      }
    } finally { busy = false; }
  });
  const start = () => {
    fixNode(document.body);
    mo.observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ATTRS });
    // window title badge
    new MutationObserver(() => { const v = translate(document.title); if (v !== document.title) document.title = v; }).observe(document.querySelector("title"), { childList: true, characterData: true, subtree: true });
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start); else start();
  // Native dialogs used by the UI.
  const oc = window.confirm.bind(window); window.confirm = (m) => oc(translate(String(m)));
  const oa = window.alert.bind(window); window.alert = (m) => oa(translate(String(m)));
})();
