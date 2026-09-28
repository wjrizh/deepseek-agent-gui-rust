import { createRequire } from 'module';
const require = createRequire(import.meta.url);
import fs from 'fs';
import os from 'os';
import path from 'path';

const PW_DIR = process.env.LIGONG_PLAYWRIGHT || path.join(os.homedir(), '.ligong-mcp/node_modules/playwright');
const { chromium } = require(PW_DIR);
const PROFILE_DIR = process.env.LIGONG_PROFILE_DIR || '/tmp/ligong-dump-profile';
const CHROMIUM_EXEC = process.env.LIGONG_CHROMIUM || path.join(os.homedir(), '.cache/ms-playwright/chromium-1223/chrome-linux64/chrome');
const STATE_FILE = process.env.LIGONG_STATE_FILE || path.join(os.homedir(), '.ligong/accounts/HJ/state.json');
const SID = process.env.SID || '24ee8a72-1592-4763-8548-45625c76addc';

const context = await chromium.launchPersistentContext(PROFILE_DIR, { headless: false, executablePath: CHROMIUM_EXEC });
if (fs.existsSync(STATE_FILE)) {
  try {
    const st = JSON.parse(fs.readFileSync(STATE_FILE, 'utf8'));
    if (Array.isArray(st.cookies) && st.cookies.length) await context.addCookies(st.cookies);
  } catch (e) { console.error('inject fail', e); }
}
const pages = context.pages();
const page = pages.length ? pages[0] : await context.newPage();

await page.goto('https://chat.deepseek.com/a/chat/s/' + SID, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(6000);

const info = await page.evaluate(() => {
  const q = (sel) => document.querySelectorAll(sel).length;
  const cls = (sel) => Array.from(document.querySelectorAll(sel)).slice(0, 12).map(e => (e.className || '').toString().slice(0, 80));
  const texts = (sel) => Array.from(document.querySelectorAll(sel)).slice(0, 12).map(e => (e.innerText || '').slice(0, 40));
  const scrollers = Array.from(document.querySelectorAll('*')).filter(e => {
    const s = getComputedStyle(e);
    return (s.overflowY === 'auto' || s.overflowY === 'scroll') && e.scrollHeight > e.clientHeight + 50;
  }).slice(0, 8).map(e => ({ tag: e.tagName, cls: (e.className||'').toString().slice(0,80), scrollH: e.scrollHeight, clientH: e.clientHeight, children: e.children.length }));
  return {
    url: location.href,
    dsMessage: q('.ds-message'),
    dsMessageClasses: cls('.ds-message'),
    dsMessageTexts: texts('.ds-message'),
    candidates: {
      '.ds-message': q('.ds-message'),
      '[class*=message]': q('[class*=message]'),
      '[class*=Message]': q('[class*=Message]'),
      '[class*=chat-item]': q('[class*=chat-item]'),
      '[class*=markdown]': q('[class*=markdown]'),
      '[data-testid]': q('[data-testid]'),
      'div[role=listitem]': q('div[role=listitem]'),
      'article': q('article'),
    },
    scrollers,
    bodyTextLen: document.body.innerText.length,
    bodyTextHead: document.body.innerText.slice(0, 300),
  };
});
console.log(JSON.stringify(info, null, 2));

const ls = await page.evaluate(() => {
  const out = {};
  for (const k of Object.keys(localStorage)) {
    const v = localStorage.getItem(k) || '';
    out[k] = v.length > 80 ? v.slice(0, 80) + '...(' + v.length + ')' : v;
  }
  return out;
});
console.log('LOCALSTORAGE', JSON.stringify(ls, null, 2));

await context.close();
