#!/usr/bin/env node

import fs from "node:fs/promises";
import path from "node:path";
import { chromium } from "playwright-core";

const PAGE_PRESETS = {
  dashboard: { path: "/", width: 1440, height: 2200 },
  status: { path: "/status", width: 1440, height: 2200 },
  scan: { path: "/scan", width: 1440, height: 2200 },
  cleanup: { path: "/cleanup", width: 1440, height: 2400 },
  dashboardMobile: { path: "/", width: 430, height: 2200, isMobile: true },
  statusMobile: { path: "/status", width: 430, height: 2200, isMobile: true },
  scanMobile: { path: "/scan", width: 430, height: 2200, isMobile: true },
  cleanupMobile: { path: "/cleanup", width: 430, height: 2400, isMobile: true },
};

function timestampLabel() {
  return new Date().toISOString().replace(/[-:]/g, "").replace(/\..+/, "").replace("T", "-");
}

function normalizeBaseUrl(raw) {
  return (raw || "http://127.0.0.1:8726").replace(/\/+$/, "");
}

function selectedPages() {
  const requested = (process.env.SYMLINKARR_CAPTURE_PAGES || "")
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);

  if (requested.length === 0) {
    return [
      "dashboard",
      "status",
      "scan",
      "cleanup",
      "dashboardMobile",
      "statusMobile",
      "scanMobile",
      "cleanupMobile",
    ];
  }

  return requested.filter((name) => PAGE_PRESETS[name]);
}

function selectedTheme() {
  const theme = (process.env.SYMLINKARR_CAPTURE_THEME || "").trim();
  return theme || null;
}

async function ensureDir(target) {
  await fs.mkdir(target, { recursive: true });
}

async function run() {
  const baseUrl = normalizeBaseUrl(process.env.SYMLINKARR_CAPTURE_BASE_URL || process.argv[2]);
  const chromiumPath = process.env.SYMLINKARR_CHROMIUM_PATH || "/usr/bin/chromium";
  const theme = selectedTheme();
  const outDir =
    process.env.SYMLINKARR_CAPTURE_DIR ||
    process.argv[3] ||
    path.join("backups", "ui-design", `capture-${timestampLabel()}`);
  const pageNames = selectedPages();

  if (pageNames.length === 0) {
    throw new Error("No valid page presets selected.");
  }

  await ensureDir(outDir);

  const browser = await chromium.launch({
    executablePath: chromiumPath,
    headless: true,
  });

  try {
    for (const pageName of pageNames) {
      const preset = PAGE_PRESETS[pageName];
      const context = await browser.newContext({
        viewport: { width: preset.width, height: preset.height },
        isMobile: !!preset.isMobile,
        deviceScaleFactor: preset.isMobile ? 2 : 1,
      });

      if (theme) {
        await context.addInitScript((themeId) => {
          try {
            window.localStorage.setItem("symlinkarr-theme", themeId);
          } catch (error) {}
        }, theme);
      }

      const page = await context.newPage();
      const url = `${baseUrl}${preset.path}`;
      const fileName = `${pageName}.png`;
      const outputPath = path.join(outDir, fileName);

      await page.goto(url, { waitUntil: "networkidle" });
      await page.screenshot({ path: outputPath, fullPage: true });
      await context.close();

      console.log(`${fileName} ${url}`);
    }
  } finally {
    await browser.close();
  }

  console.log(`Saved screenshots to ${outDir}`);
}

run().catch((error) => {
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
});
