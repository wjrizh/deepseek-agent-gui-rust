import { createRequire } from 'module';
const require = createRequire(import.meta.url);
import fs from 'fs';
import os from 'os';
import path from 'path';
const PW_DIR = process.env.LIGONG_PLAYWRIGHT || path.join(os.homedir(), '.ligong-mcp/node_modules/playwright');
const { chromium } = require(PW_DIR);
import readline from 'readline';
const PROFILE_DIR = process.env.LIGONG_PROFILE_DIR;
const HEADLESS = process.env.LIGONG_HEADLESS === '1';
const CHROMIUM_EXEC = process.env.LIGONG_CHROMIUM;
if (!PROFILE_DIR) { console.error('LIGONG_PROFILE_DIR not set'); process.exit(1); }

let context, page;
let captured = null;
let stopRequested = false;

function log(...a) { console.error('[bridge]', ...a); }

// 停不下来 = 还在生成。对齐原文的多个选择器。
function stopButton() {
  return page.locator(
    "div[role='button']:has-text('停止生成'), .ds-stop-button, [aria-label*='Stop' i], svg.stop-icon"
  );
}

async function stopVisible() {
  try { return await stopButton().isVisible(); } catch { return false; }
}

// 按需切换 DeepThink / Search toggle（prompt 前调用）
async function setToggle(label, want) {
  const loc = page.locator(`div.ds-toggle-button:has-text('${label}')`);
  if (!(await loc.count())) { log('warn: toggle not found:', label); return; }
  const el = loc.first();
  const pressed = (await el.getAttribute('aria-pressed')) === 'true';
  if (pressed !== !!want) {
    await el.click();
    await page.waitForTimeout(400);
  }
  log('toggle', label, '=>', want, '(now', (await el.getAttribute('aria-pressed')) + ')');
}

async function applyToggles(thinking, search) {
  await setToggle('DeepThink', thinking);
  await setToggle('Search', search);
}

async function init() {
  const launchOpts = { headless: HEADLESS };
  if (CHROMIUM_EXEC) launchOpts.executablePath = CHROMIUM_EXEC;
  log('launching chromium headless=', HEADLESS, 'profile=', PROFILE_DIR, 'exec=', CHROMIUM_EXEC || '(bundled)');
  context = await Promise.race([
    chromium.launchPersistentContext(PROFILE_DIR, launchOpts),
    new Promise((_, rej) => setTimeout(() => rej(new Error('launchPersistentContext timeout (90s)')), 90000)),
  ]);
  log('chromium launched');

  let injected = false;
  const stateFile = process.env.LIGONG_STATE_FILE;
  if (stateFile && fs.existsSync(stateFile)) {
    try {
      const state = JSON.parse(fs.readFileSync(stateFile, 'utf8'));
      if (Array.isArray(state.cookies) && state.cookies.length) {
        await context.addCookies(state.cookies);
        injected = true;
        log('injected cookies:', state.cookies.length);
      }
    } catch (e) { log('state inject failed:', String((e && e.message) || e)); }
  }

  const pages = context.pages();
  page = pages.length ? pages[0] : await context.newPage();
  log('page ready');

  page.on('response', (resp) => {
    const u = resp.url();
    if (u.includes('/chat/completion') && resp.request().method() === 'POST') {
      if (!captured) { captured = resp; log('captured SSE:', u); }
      if (process.env.LIGONG_DUMP_SSE) {
        resp.text().then((t) => {
          try { fs.appendFileSync(process.env.LIGONG_DUMP_SSE, t + '\n\n=====RESP=====\n\n'); } catch {}
        }).catch(() => {});
      }
    }
  });

  log('navigating to chat.deepseek.com');
  await page.goto('https://chat.deepseek.com/', { waitUntil: 'domcontentloaded', timeout: 60000 });
  log('navigated, url=', page.url());
  if (injected) {
    await page.reload({ waitUntil: 'domcontentloaded', timeout: 60000 }).catch(() => {});
  }
  try {
    await page.waitForSelector('textarea', { timeout: 30000 });
  } catch {
    if (!page.isClosed()) {
      await page.goto('https://chat.deepseek.com/', { waitUntil: 'domcontentloaded', timeout: 60000 }).catch(() => {});
      try { await page.waitForSelector('textarea', { timeout: 30000 }); } catch {}
    }
  }
  log('ready, url =', page.url());
}

function parseSSE(rawSse) {
  const fragments = [];
  let currentPath = '';
  for (let line of rawSse.split('\n')) {
    line = line.trim();
    if (!line.startsWith('data: ')) continue;
    const dataStr = line.slice(6).trim();
    if (!dataStr || dataStr === '[DONE]') continue;
    let data;
    try { data = JSON.parse(dataStr); } catch { continue; }
    if (typeof data !== 'object' || data === null) continue;
    if ('p' in data) currentPath = data.p;
    if ('v' in data && typeof data.v === 'object' && !Array.isArray(data.v)) {
      const resp = data.v.response || {};
      if (Array.isArray(resp.fragments)) { fragments.push(...resp.fragments); continue; }
    }
    if (currentPath === 'response/fragments' && Array.isArray(data.v)) {
      fragments.push(...data.v); continue;
    }
    if (currentPath === 'response/fragments/-1/content' && typeof data.v === 'string') {
      log('[SSE-DEBUG] incremental hit, currentPath=', currentPath, 'prevType=', fragments[fragments.length-1]?.type);
      if (!fragments.length) {
        fragments.push({ type: 'RESPONSE', content: '' });
      }
      const last = fragments[fragments.length - 1];
      if (typeof last.content !== 'string') last.content = '';
      last.content += data.v;
    }
  }
  {
    const types = {};
    for (const frag of fragments) {
      const t = String(frag.type || '').toUpperCase();
      types[t] = (types[t] || 0) + 1;
    }
    log('[SSE-DEBUG] fragment types:', JSON.stringify(types), 'total=', fragments.length);
  }
  let full = '', think = '';
  for (const frag of fragments) {
    const t = String(frag.type || '').toUpperCase();
    const c = String(frag.content || '');
    if (t === 'THINK') think += c;
    else if (t === 'RESPONSE' || t === 'TEXT') full += c;
    else if (t !== 'SEARCH' && t !== 'PLUGIN') full += c;
  }
  return { think, answer: full };
}

function sessionIdFromUrl(u) {
  const m = String(u || '').match(/\/a\/chat\/s\/([0-9a-fA-F-]{8,})/);
  return m ? m[1] : null;
}

// 合法会话 id：DeepSeek 服务端 UUID（8-4-4-4-12）
function isValidSessionId(sid) {
  return /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/.test(String(sid || ''));
}

async function openSession(sid) {
  if (!sid || !isValidSessionId(sid)) return;
  if (sessionIdFromUrl(page.url()) === sid) return;
  await page.goto('https://chat.deepseek.com/a/chat/s/' + sid, {
    waitUntil: 'domcontentloaded', timeout: 60000,
  });
  try { await page.waitForSelector('textarea', { timeout: 30000 }); } catch {}
  await page.waitForTimeout(800);
  log('opened session', sid, 'url=', page.url());
}

async function gotoBlank() {
  await page.goto('https://chat.deepseek.com/', { waitUntil: 'domcontentloaded', timeout: 60000 });
  try { await page.waitForSelector('textarea', { timeout: 30000 }); } catch {}
  await page.waitForTimeout(500);
}

async function messageCount(sid) {
  if (sid) await openSession(sid);
  return await page.evaluate(() => {
    const el = document.querySelector('.ds-virtual-list');
    if (el) {
      const key = Object.keys(el).find(k => k.startsWith('__reactFiber') || k.startsWith('__reactProps') || k.startsWith('__reactInternalInstance'));
      if (key) {
        let node = el[key];
        for (let i = 0; i < 40 && node; i++) {
          const mp = node.memoizedProps;
          if (mp && Array.isArray(mp.items)) return mp.items.length;
          node = node.return || node.child;
        }
      }
    }
    return document.querySelectorAll('.ds-message').length;
  });
}

async function deleteSession(sid) {
  if (!sid || !isValidSessionId(sid)) return false;
  try {
    await gotoBlank();
    await page.waitForTimeout(800);
    const link = page.locator('a[href="/a/chat/s/' + sid + '"]').first();
    if (!(await link.count())) { log('delete: session not in list', sid); return false; }
    await link.scrollIntoViewIfNeeded().catch(() => {});
    await link.hover();
    await page.waitForTimeout(500);
    await link.locator('[role="button"]').first().click({ timeout: 5000 });
    await page.waitForTimeout(800);
    const delOpt = page.locator('.ds-dropdown-menu-option', { hasText: 'Delete' }).first();
    if (!(await delOpt.count())) { log('delete: menu item not found'); return false; }
    await delOpt.click();
    await page.waitForTimeout(800);
    const confirm = page.locator('.ds-modal [class*="ds-button"][class*="error"]', { hasText: 'Delete chat' }).first();
    if (await confirm.count()) {
      await confirm.click({ timeout: 5000 });
    } else {
      await page.locator('.ds-modal-content__footer [class*="ds-button"]').last().click({ timeout: 5000 });
    }
    await page.waitForTimeout(1200);
    log('delete: done', sid);
    return true;
  } catch (e) {
    log('delete error', String((e && e.message) || e));
    return false;
  }
}

async function listSessions() {
  await gotoBlank();
  await page.waitForTimeout(1500);
  return await page.evaluate(() => {
    const out = [];
    document.querySelectorAll('a[href^="/a/chat/s/"]').forEach(a => {
      const m = a.getAttribute('href').match(/\/a\/chat\/s\/([0-9a-fA-F-]+)/);
      if (!m) return;
      const title = (a.innerText || '').trim().split('\n')[0];
      out.push({ id: m[1], title });
    });
    return out;
  });
}

async function uploadFile(paths) {
  const list = Array.isArray(paths) ? paths : [paths];
  if (!list.length) return 0;
  const fi = page.locator('input[type="file"]').first();
  if (!(await fi.count())) { log('upload: no file input'); return 0; }
  await fi.setInputFiles(list);
  for (let i = 0; i < 30; i++) {
    await page.waitForTimeout(500);
    const done = await page.evaluate((names) => {
      const body = document.body.innerText;
      return names.every(n => body.includes(n));
    }, list.map(p => p.split('/').pop()));
    if (done) break;
  }
  log('upload: done', list.length);
  return list.length;
}

// 已上传文件卡片（DOM 结构随前端版本变化，用多组候选类名兜底）
function fileCardSelectors() {
  return [
    'div.cd314545',
    '[class*="file-card"]',
    '[class*="file-item"]',
    '[class*="attachment"]',
  ];
}

async function listFiles() {
  return await page.evaluate((sels) => {
    const seen = new Set();
    const out = [];
    for (const sel of sels) {
      for (const card of document.querySelectorAll(sel)) {
        if (seen.has(card)) continue;
        seen.add(card);
        const nameEl = card.querySelector('div.e70accd6, [class*="file-name"], [class*="name"]');
        const name = (nameEl ? nameEl.textContent : card.innerText) || '';
        const trimmed = name.trim().split('\n')[0];
        if (trimmed) out.push(trimmed);
      }
    }
    return out;
  }, fileCardSelectors());
}

async function removeFile(name) {
  const sels = fileCardSelectors();
  for (const sel of sels) {
    const card = page.locator(sel, { hasText: name }).first();
    if (await card.count()) {
      const btn = card.locator('div._8402d8c, [class*="close"], [class*="del"], [class*="remove"]').first();
      if (await btn.count()) {
        await btn.click({ timeout: 5000 }).catch(() => {});
        await page.waitForTimeout(500);
        return true;
      }
    }
  }
  return false;
}

async function clearFiles() {
  const sels = fileCardSelectors();
  let n = 0;
  for (let i = 0; i < 60; i++) {
    let clicked = false;
    for (const sel of sels) {
      const card = page.locator(sel).first();
      if (!(await card.count())) continue;
      const btn = card.locator('div._8402d8c, [class*="close"], [class*="del"], [class*="remove"]').first();
      if (!(await btn.count())) continue;
      await btn.click({ timeout: 5000 }).catch(() => {});
      await page.waitForTimeout(400);
      clicked = true;
      n++;
      break;
    }
    if (!clicked) break;
  }
  return n;
}

async function ask(prompt, thinking, search, sessionId, parentId, files) {
  captured = null;
  stopRequested = false;
  if (sessionId && !isValidSessionId(sessionId)) {
    log('warn: invalid session id, starting fresh:', sessionId);
    sessionId = null;
  }
  if (sessionId) await openSession(sessionId); else await gotoBlank();
  await applyToggles(thinking, search);
  if (files && files.length) await uploadFile(files);
  const box = await page.waitForSelector('textarea', { timeout: 5000 });
  await box.fill('');
  await box.fill(prompt);
  await page.keyboard.press('Enter');
  await page.waitForTimeout(1500);

  let lastLen = 0, stable = 0;
  let sseDone = false;
  for (let i = 0; i < 300; i++) {
    await page.waitForTimeout(1000);

    if (stopRequested) {
      try {
        const sb = stopButton();
        if (await sb.count() && await sb.isVisible()) await sb.click();
      } catch {}
      stopRequested = false;
      break;
    }

    if (captured && !sseDone) {
      try {
        const done = await Promise.race([
          captured.finished().then(() => true).catch(() => false),
          new Promise(r => setTimeout(() => r(false), 50)),
        ]);
        if (done) { sseDone = true; log('SSE finished at iter', i); }
      } catch {}
    }
    if (sseDone) {
      if (!(await stopVisible())) break;
      return;
    }

    const len = await page.evaluate(() => {
      const m = document.querySelectorAll('.ds-message');
      return m.length ? m[m.length - 1].innerText.length : 0;
    });
    if (len > 0 && len === lastLen) stable++; else { stable = 0; lastLen = len; }
    if (await stopVisible()) { stable = 0; continue; }
    if (stable >= 4 && captured) break;
  }

  let responseMessageId = null;
  let newSessionId = null;
  if (captured) {
    let raw = '';
    try {
      raw = await Promise.race([
        captured.text(),
        new Promise((_, rej) => setTimeout(() => rej(new Error('sse body timeout')), 60000)),
      ]);
    } catch (e) {
      log('sse body read failed:', String((e && e.message) || e));
      raw = '';
    }
    const parsed = parseSSE(raw);
    const mid = raw.match(/"response_message_id"\s*:\s*(\d+)/);
    if (mid) responseMessageId = Number(mid[1]);
    newSessionId = sessionIdFromUrl(page.url());
    let reqBodySid = null;
    try {
      const body = captured.request().postData() || '';
      const m = body.match(/"chat_session_id"\s*:\s*"([^"]+)"/);
      if (m && m[1]) reqBodySid = m[1];
    } catch {}
    if (!newSessionId && reqBodySid) newSessionId = reqBodySid;
    if (parsed.think || parsed.answer) {
      return { ...parsed, responseMessageId, sessionId: newSessionId };
    }
  }

  const extracted = await Promise.race([
    page.evaluate(() => {
    const msgs = document.querySelectorAll('.ds-message, .ds-message-bubble');
    if (msgs.length === 0) return '';
    const lastMsg = msgs[msgs.length - 1];
    function extract(node) {
      if (node.nodeType === 3) return node.textContent;
      if (node.nodeType !== 1) return '';
      const tag = node.tagName.toUpperCase();
      if (tag === 'BR') return '\n';
      if (node.classList && (node.classList.contains('ds-markdown--think') || node.classList.contains('ds-think-content') || node.classList.contains('ds-thought-content'))) {
        return '\n<think>\n' + node.innerText + '\n</think>\n';
      }
      if (tag === 'PRE') {
        const code = node.querySelector('code');
        const content = code ? code.innerText : node.innerText;
        return '\n```\n' + content.trim() + '\n```\n';
      }
      let res = '';
      for (const child of node.childNodes) res += extract(child);
      if (['P', 'DIV', 'LI', 'H1', 'H2', 'H3', 'TR'].includes(tag)) return '\n' + res + '\n';
      if (tag === 'STRONG' || tag === 'B') return '__' + res + '__';
      if (tag === 'EM' || tag === 'I') return '_' + res + '_';
      if (tag === 'CODE') return '`' + res + '`';
      if (tag === 'DEL' || tag === 'S') return '~~' + res + '~~';
      return res;
    }
    return extract(lastMsg).replace(/\n{3}/g, '\n\n').trim();
    }),
    new Promise((r) => setTimeout(() => r(''), 10000)),
  ]);
  if (extracted) {
    const curSid = sessionIdFromUrl(page.url());
    const m = extracted.match(/<think>([\s\S]*?)<\/think>/);
    if (m) return { think: m[1].trim(), answer: extracted.replace(/<think>[\s\S]*?<\/think>/, '').trim(), responseMessageId: null, sessionId: curSid };
    return { think: '', answer: extracted, responseMessageId: null, sessionId: curSid };
  }

  throw new Error('no SSE captured');
}

const rl = readline.createInterface({ input: process.stdin });
await init();
process.stdout.write(JSON.stringify({ ok: true, ready: true }) + '\n');

// toggle 请求走独立即时通道：即便 ask 正在跑，也能立刻作用到浏览器页面。
// 其余请求进串行队列，避免并发操作页面。
let chain = Promise.resolve();
function enqueue(fn) {
  chain = chain.then(fn, fn);
  return chain;
}

async function handleToggle(req) {
  // 单向通知，不回写响应：避免污染在途 ask 的 stdout 响应流。
  try {
    await applyToggles(!!req.toggles.thinking, !!req.toggles.search);
  } catch (e) {
    log('toggle failed:', String((e && e.message) || e));
  }
}

async function handleSerial(req, rid) {
  try {
    if (req.stop) { stopRequested = true; return; }
    if (req.quit) {
      try { await context?.close(); } catch {}
      process.exit(0);
    }
    if (req.message_count !== undefined) {
      const n = await messageCount(req.session_id || null);
      process.stdout.write(JSON.stringify({ ok: true, count: n , req_id: rid}) + '\n');
      return;
    }
    if (req.delete_session) {
      const ok = await deleteSession(req.session_id || null);
      process.stdout.write(JSON.stringify({ ok: true, deleted: ok , req_id: rid}) + '\n');
      return;
    }
    if (req.list_sessions) {
      const sessions = await listSessions();
      process.stdout.write(JSON.stringify({ ok: true, sessions , req_id: rid}) + '\n');
      return;
    }
    if (req.upload_files) {
      if (req.session_id && isValidSessionId(req.session_id)) await openSession(req.session_id);
      else if (!sessionIdFromUrl(page.url())) await gotoBlank();
      const n = await uploadFile(req.upload_files);
      process.stdout.write(JSON.stringify({ ok: true, uploaded: n , req_id: rid}) + '\n');
      return;
    }
    if (req.list_files) {
      if (req.session_id && isValidSessionId(req.session_id)) await openSession(req.session_id);
      const files = await listFiles();
      process.stdout.write(JSON.stringify({ ok: true, files , req_id: rid}) + '\n');
      return;
    }
    if (req.remove_file) {
      if (req.session_id && isValidSessionId(req.session_id)) await openSession(req.session_id);
      const ok = await removeFile(req.remove_file);
      process.stdout.write(JSON.stringify({ ok: true, removed: ok , req_id: rid}) + '\n');
      return;
    }
    if (req.clear_files) {
      if (req.session_id && isValidSessionId(req.session_id)) await openSession(req.session_id);
      const n = await clearFiles();
      process.stdout.write(JSON.stringify({ ok: true, cleared: n , req_id: rid}) + '\n');
      return;
    }
    const result = await ask(req.prompt, !!req.thinking, !!req.search, req.session_id || null, req.parent_message_id != null ? req.parent_message_id : null, req.files || null);
    process.stdout.write(JSON.stringify({
      ok: true,
      text: result.answer,
      think: result.think,
      message_id: result.responseMessageId != null ? result.responseMessageId : null,
      session_id: result.sessionId || null,
      req_id: rid,
    }) + '\n');
  } catch (e) {
    process.stdout.write(JSON.stringify({ ok: false, error: String((e && e.message) || e) , req_id: rid}) + '\n');
  }
}

rl.on("line", (line) => {
  if (!line.trim()) return;
  let rid = null;
  let req;
  try {
    req = JSON.parse(line);
  } catch (e) {
    process.stdout.write(JSON.stringify({ ok: false, error: String((e && e.message) || e) }) + "\n");
    return;
  }
  rid = req.req_id !== undefined ? req.req_id : null;
  if (req.toggles) {
    void handleToggle(req);
    return;
  }
  enqueue(() => handleSerial(req, rid));
});
