#!/usr/bin/env bash
# Shoot the whole README set (light theme — set it in the app first) into $1.
# Every shot is preceded by a "settle"
# (the wait predicate has to be true on 4 consecutive polls) so the WebKit
# snapshot never races React's paint.
set -u
OUT="$1"; mkdir -p "$OUT"
cd "$(dirname "$0")/../ui-verify"
SETTLE='wait|(window.__s = (window.__s ? window.__s : 0) + 1) > 4|5000'
RESET='eval|window.__s = 0'
TITLE='document.querySelector(".titlebar__title").textContent'
BACK='eval|(() => { const b = document.querySelector(".toolbar .btn--ghost"); if (b) b.click(); return !!b; })()'

node ui.mjs do <<EOF
$BACK
click|text=Chats
wait|/Chats/.test($TITLE) && /Review PR #518/.test(document.body.textContent)|15000
eval|(() => { const el = [...document.querySelectorAll(".listrow")].find(e => /Review PR #518/.test(e.textContent)); el.click(); return 1; })()
wait|document.querySelectorAll(".toolcall").length > 1 && /safe to merge/.test(document.body.textContent)|15000
$RESET
$SETTLE
shot|$OUT/chats.png
click|text=Agents
wait|/Agents/.test($TITLE) && /Code Reviewer/.test(document.body.textContent) && document.querySelectorAll("textarea").length > 0 && /get_pull_diff/.test(document.body.textContent)|15000
eval|document.querySelector(".scroll").scrollTop = 0
$RESET
$SETTLE
shot|$OUT/agents.png
eval|(() => { const h = [...document.querySelectorAll(".formsec__title")].find(e => e.textContent.trim() === "Capabilities"); h.scrollIntoView({block:"start"}); return 1; })()
$RESET
$SETTLE
shot|$OUT/agent-builder.png
click|text=Integrations
wait|/Integrations/.test($TITLE) && /17 tools/.test(document.body.textContent)|15000
eval|(() => { const el = [...document.querySelectorAll(".listrow")].find(e => /^GitHub \(acme\)/.test(e.textContent.trim())); el.click(); return 1; })()
wait|document.querySelector("input[value='GitHub (acme)']") !== null && [...document.querySelectorAll(".listrow--active")].some(e => /GitHub/.test(e.textContent))|10000
$RESET
$SETTLE
shot|$OUT/integrations.png
click|text=Scheduled Tasks
wait|/Scheduled Tasks/.test($TITLE) && /Draft release notes/.test(document.body.textContent)|15000
eval|(() => { const el = [...document.querySelectorAll(".listrow")].find(e => /^Draft release notes/.test(e.textContent.trim())); el.click(); return 1; })()
wait|document.querySelector("input[value='Draft release notes']") !== null && [...document.querySelectorAll(".listrow--active")].some(e => /Draft release notes/.test(e.textContent)) && [...document.querySelectorAll(".segmented__item--active")].some(e => e.textContent.trim() === "Interval")|10000
$RESET
$SETTLE
shot|$OUT/tasks.png
click|text=Job History
wait|/Job History/.test($TITLE) && document.querySelectorAll(".runrow, tbody tr").length > 3|15000
$RESET
$SETTLE
shot|$OUT/job-history.png
click|text=Sessions
wait|/Sessions/.test($TITLE) && /301 sessions/.test(document.body.textContent) && document.querySelectorAll("tbody tr").length > 20|15000
$RESET
$SETTLE
shot|$OUT/sessions-list.png
eval|(() => { const tr = [...document.querySelectorAll("tbody tr")].find(r => /Harden the file upload validation/.test(r.textContent) && /fix\/upload-validation/.test(r.textContent)); tr.dispatchEvent(new MouseEvent("click",{bubbles:true})); tr.dispatchEvent(new MouseEvent("dblclick",{bubbles:true})); return "ok"; })()
wait|!/Reading transcript/.test(document.body.textContent) && document.querySelectorAll(".toolcall").length > 3 && /10 msgs/.test(document.body.textContent)|20000
$RESET
$SETTLE
shot|$OUT/session-detail.png
click|button[title="Toggle Inspector"]
eval|(() => { const names = [...document.querySelectorAll(".toolcall__name")]; const pick = names.filter(e => ["Agent","Bash"].includes(e.textContent.trim())).slice(0,2); pick.forEach(e => { const btn = e.closest("button") ? e.closest("button") : e.closest(".toolcall"); btn.click(); }); return pick.length; })()
eval|(() => { const tx = document.querySelector(".transcript.scroll"); const agent = [...tx.querySelectorAll(".toolcall__name")].find(e => e.textContent.trim() === "Agent"); const msg = agent.closest(".msg") ? agent.closest(".msg") : agent.parentElement.parentElement.parentElement; msg.scrollIntoView({block: "start"}); tx.scrollTop -= 90; return tx.scrollTop; })()
$RESET
$SETTLE
shot|$OUT/session-journey.png
click|button[title="Toggle Inspector"]
$BACK
click|text=Token Usage
wait|/Token Usage/.test($TITLE) && !/Loading analytics/.test(document.body.textContent) && /Cost by model/.test(document.body.textContent)|25000
eval|document.querySelector(".dash.scroll").scrollTop = 0
$RESET
$SETTLE
shot|$OUT/token-usage.png
eval|(() => { const d = document.querySelector(".dash.scroll"); d.scrollTop = d.scrollHeight; return d.scrollTop; })()
$RESET
$SETTLE
shot|$OUT/cost-by-model.png
click|text=General Usage
wait|/General Usage/.test($TITLE) && !/Loading analytics/.test(document.body.textContent) && /Weekly rhythm/.test(document.body.textContent)|25000
eval|document.querySelector(".dash.scroll").scrollTop = 0
$RESET
$SETTLE
shot|$OUT/general-usage.png
eval|(() => { const h = [...document.querySelectorAll(".dash.scroll *")].find(e => e.children.length <= 3 && /^Weekly rhythm/.test(e.textContent.trim())); let card = h.closest(".card"); if (!card) card = h; card.scrollIntoView({block: "start"}); document.querySelector(".dash.scroll").scrollTop -= 8; return 1; })()
$RESET
$SETTLE
shot|$OUT/activity-heatmap.png
eval|(() => { const d = document.querySelector(".dash.scroll"); d.scrollTop = d.scrollHeight; return d.scrollTop; })()
$RESET
$SETTLE
shot|$OUT/top-sessions.png
click|text=Insights
wait|/Insights/.test($TITLE) && !/Loading analytics/.test(document.body.textContent) && /Tool call attribution/.test(document.body.textContent)|25000
eval|document.querySelector(".dash.scroll").scrollTop = 0
$RESET
$SETTLE
shot|$OUT/insights.png
eval|(() => { const d = document.querySelector(".dash.scroll"); d.scrollTop = d.scrollHeight; return d.scrollTop; })()
$RESET
$SETTLE
shot|$OUT/insights-breakdowns.png
click|text=Settings
wait|/Settings/.test($TITLE)|10000
click|text=Pricing
wait|/claude-opus/.test(document.body.textContent)|15000
eval|(() => { const cell = [...document.querySelectorAll("td, div, span")].find(e => e.children.length === 0 && e.textContent.trim() === "Claude 3.5 Sonnet"); const sc = document.querySelector(".scroll"); cell.closest("tr").scrollIntoView({block:"start"}); sc.scrollTop -= 4; return sc.scrollTop; })()
$RESET
$SETTLE
shot|$OUT/settings-pricing.png
click|text=Data
wait|document.body.textContent.indexOf("Idle gap threshold") >= 0 && document.body.textContent.indexOf("scratch") >= 0|15000
$RESET
$SETTLE
shot|$OUT/settings-data.png
click|text=Claude
wait|document.body.textContent.indexOf("Claude") >= 0 && document.querySelectorAll("input, textarea").length > 1|15000
$RESET
$SETTLE
shot|$OUT/settings-claude.png
EOF
