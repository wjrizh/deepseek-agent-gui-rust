import { createRequire } from 'module';
const require = createRequire(import.meta.url);
import fs from 'fs';
import os from 'os';
import path from 'path';

const PW_DIR = process.env.LIGONG_PLAYWRIGHT || path.join(os.homedir(), '.ligong-mcp/node_modules/playwright');
const { chromium } = require(PW_DIR);

const PROFILE_DIR = process.env.LIGONG_PROFILE_DIR;
const STATE_FILE = process.env.LIGONG_STATE_FILE;
const CHROMIUM_EXEC = process.env.LIGONG_CHROMIUM || '';
if (!PROFILE_DIR) { console.error('LIGONG_PROFILE_DIR not set'); process.exit(1); }

const sessionName = process.argv[2] || 'account';
console.error('== 登录流程 ==');
console.error('账号:', sessionName);
console.error('Profile:', PROFILE_DIR);

const launchOpts = { headless: false };
if (CHROMIUM_EXEC) launchOpts.executablePath = CHROMIUM_EXEC;

const context = await chromium.launchPersistentContext(PROFILE_DIR, launchOpts);
const pages = context.pages();
const page = pages.length ? pages[0] : await context.newPage();

await page.goto('https://chat.deepseek.com/', { waitUntil: 'domcontentloaded', timeout: 60000 });

let ok = false;
for (let i = 0; i < 600; i++) {
  await page.waitForTimeout(1000);
  try {
    const url = page.url();
    if (url.includes('/sign_in') || url.includes('/login')) continue;
    const hasBox = await page.locator('textarea').count();
    if (hasBox) { ok = true; break; }
  } catch {}
}

if (ok) {
  await page.waitForTimeout(1500);
  if (STATE_FILE) {
    try {
      fs.mkdirSync(path.dirname(STATE_FILE), { recursive: true });
      await context.storageState({ path: STATE_FILE });
      console.error('已保存登录态:', STATE_FILE);
    } catch (e) { console.error('保存登录态失败:', String((e && e.message) || e)); }
  }
  console.log('LOGIN_OK');
} else {
  console.error('LOGIN_TIMEOUT');
}

await context.close();
process.exit(ok ? 0 : 2);
