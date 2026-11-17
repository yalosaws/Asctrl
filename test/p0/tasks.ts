// P0's task set: real tasks in a browser, Windows Settings, Office and VS Code (an Electron app), 10 each.
// Each task is checked by what it left behind (a file, a form the local site received) or by its answer
// against the truth read from the system; never by asking the model whether it worked.
// WeChat is left out: its tasks would send messages to real people.

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { answers, type Site } from "./site.ts";

export type Area = "browser" | "settings" | "office" | "vscode";
export interface Ctx { work: string; site: Site }
export interface Task {
  id: string;
  area: Area;
  prompt: (c: Ctx) => string;
  setup?: (c: Ctx) => void;
  /** answer: what followed "ANSWER:" in the model's last reply */
  check: (c: Ctx, answer: string) => string | true;
}

/** Runs PowerShell and returns what it printed */
export function ps(script: string): string {
  return execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `[Console]::OutputEncoding=[Text.Encoding]::UTF8; ${script}`], { encoding: "utf8", timeout: 60_000 }).trim();
}
/** The text of one part of an Office file (they are zip archives of XML) */
function part(file: string, entry: string): string {
  if (!existsSync(file)) throw new Error(`${path.basename(file)} was not saved`);
  return ps(`Add-Type -A System.IO.Compression.FileSystem; $z=[IO.Compression.ZipFile]::OpenRead('${file}'); try { $e=$z.Entries | ? FullName -like '${entry}'; foreach($x in $e){ $r=New-Object IO.StreamReader($x.Open()); $r.ReadToEnd(); $r.Close() } } finally { $z.Dispose() }`);
}
const plain = (xml: string) => xml.replace(/<w:tab\/>/g, "\t").replace(/<\/w:p>/g, "\n").replace(/<[^>]+>/g, "").replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&apos;/g, "'");
const norm = (s: string) => s.toLowerCase().replace(/[\s,]+/g, " ").trim();
const has = (hay: string, ...needles: string[]) => {
  const miss = needles.filter((n) => !norm(hay).includes(norm(n)));
  return miss.length ? `missing ${miss.map((m) => JSON.stringify(m)).join(", ")}` : true;
};
const read = (f: string) => {
  if (!existsSync(f)) throw new Error(`${path.basename(f)} is missing`);
  return readFileSync(f, "utf8");
};
const form = (c: Ctx, name: string) => {
  const f = c.site.got.get(name);
  if (!f) throw new Error(`the site received no "${name}" form`);
  return f;
};
const eq = (got: string | null, want: string) => (got?.trim() === want ? true : `${JSON.stringify(got)} instead of ${JSON.stringify(want)}`);
const all = (...r: (string | true)[]) => r.find((x) => x !== true) ?? true;

/** Excel's shared strings and the cells of the first sheet, as "A1" -> shown text or formula */
function cells(file: string): Map<string, { v: string; f?: string }> {
  const shared = [...part(file, "xl/sharedStrings.xml").matchAll(/<si>([\s\S]*?)<\/si>/g)].map((m) => plain(m[1]));
  const sheet = part(file, "xl/worksheets/sheet1.xml");
  const out = new Map<string, { v: string; f?: string }>();
  for (const m of sheet.matchAll(/<c r="([A-Z]+\d+)"([^>]*?)(?:\/>|>([\s\S]*?)<\/c>)/g)) {
    const inner = m[3] ?? "";
    const v = /<v>([^<]*)<\/v>/.exec(inner)?.[1] ?? plain(/<is>([\s\S]*?)<\/is>/.exec(inner)?.[1] ?? "");
    const f = /<f[^>]*>([^<]*)<\/f>/.exec(inner)?.[1];
    out.set(m[1], { v: /t="s"/.test(m[2]) ? shared[Number(v)] : v, f });
  }
  return out;
}
const cell = (c: Map<string, { v: string; f?: string }>, r: string) => c.get(r)?.v ?? "";

const SAVE = (c: Ctx, name: string) => `Save it as ${path.join(c.work, name)}`;

export const TASKS: Task[] = [
  // ---------- Browser: a local site (its address changes each run) and one public page
  {
    id: "b01-contact", area: "browser",
    prompt: (c) => `In a web browser, open ${c.site.url}/contact and send a message with name "Dana Park", email "dana@example.com" and message "Please call me back on Friday."`,
    check: (c) => { const f = form(c, "contact"); return all(eq(f.get("name"), "Dana Park"), eq(f.get("email"), "dana@example.com"), eq(f.get("message"), "Please call me back on Friday.")); },
  },
  {
    id: "b02-login-report", area: "browser",
    prompt: (c) => `In a web browser, sign in at ${c.site.url}/login with username "demo" and password "p0-test-pass" (a test account on a local test site), then download the report for February.`,
    check: (c) => eq(form(c, "report").get("month"), "February"),
  },
  {
    id: "b03-shop", area: "browser",
    prompt: (c) => `In a web browser, go to ${c.site.url}/shop, put 3 Blue mugs and 1 Blue bowl in the cart, and check out with the name "Sam Lee".`,
    check: (c) => { const f = form(c, "order"); return all(eq(f.get("cart") ? JSON.stringify(JSON.parse(f.get("cart")!), Object.keys(JSON.parse(f.get("cart")!)).sort()) : null, JSON.stringify({ "Blue bowl": 1, "Blue mug": 3 })), eq(f.get("name"), "Sam Lee")); },
  },
  {
    id: "b04-top-salary", area: "browser",
    prompt: (c) => `Open ${c.site.url}/people in a web browser. Who has the highest salary? Answer with the person's full name.`,
    check: (_c, a) => has(a, answers.topSalary),
  },
  {
    id: "b05-catalog", area: "browser",
    prompt: (c) => `Open ${c.site.url}/catalog in a web browser and find the price of the Copper kettle (the catalog has several pages). Answer with the price only, like 12.99.`,
    check: (_c, a) => has(a, answers.kettle),
  },
  {
    id: "b06-prefs", area: "browser",
    prompt: (c) => `In a web browser, on ${c.site.url}/prefs set the language to Deutsch, subscribe to the weekly digest but not to special offers, choose the dark theme, and save.`,
    check: (c) => { const f = form(c, "prefs"); return all(eq(f.get("lang"), "Deutsch"), eq(f.getAll("news").join(","), "weekly"), eq(f.get("theme"), "dark")); },
  },
  {
    id: "b07-booking", area: "browser",
    prompt: (c) => `In a web browser, book a table at ${c.site.url}/booking for 5 guests at 20:00 on 24 December 2026.`,
    check: (c) => { const f = form(c, "booking"); return all(eq(f.get("date"), "2026-12-24"), eq(f.get("guests"), "5"), eq(f.get("time"), "20:00")); },
  },
  {
    id: "b08-cookie-banner", area: "browser",
    prompt: (c) => `In a web browser, subscribe to the newsletter at ${c.site.url}/news with the email "news@example.com". Reject non-essential cookies if asked.`,
    check: (c) => eq(form(c, "subscribe").get("email"), "news@example.com"),
  },
  {
    id: "b09-iframe", area: "browser",
    prompt: (c) => `In a web browser, open a support ticket at ${c.site.url}/embed with the subject "Printer offline" and high priority.`,
    check: (c) => { const f = form(c, "ticket"); return all(eq(f.get("subject"), "Printer offline"), eq(f.get("priority"), "High")); },
  },
  {
    id: "b10-public-page", area: "browser",
    prompt: () => `Open https://www.iana.org/help/example-domains in a web browser. Which two second-level example domain names (besides example.com) does the page list? Answer with the two names.`,
    check: (_c, a) => has(a, "example.net", "example.org"),
  },

  // ---------- Settings: read-only questions, answered from the Settings app, checked against the system
  {
    id: "s01-device-name", area: "settings",
    prompt: () => `Using the Windows Settings app, find this computer's device name. Answer with the name.`,
    check: (_c, a) => has(a, ps("$env:COMPUTERNAME")),
  },
  {
    id: "s02-windows-version", area: "settings",
    prompt: () => `Using the Windows Settings app, find which version of Windows this is (the version like 24H2, not the build). Answer with the version.`,
    check: (_c, a) => has(a, ps("(Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion').DisplayVersion")),
  },
  {
    id: "s03-ram", area: "settings",
    prompt: () => `Using the Windows Settings app, find how much installed RAM this computer has. Answer with the number of GB.`,
    check: (_c, a) => {
      const gb = Number(ps("[math]::Round((Get-CimInstance Win32_PhysicalMemory | Measure-Object Capacity -Sum).Sum / 1GB)"));
      const said = Number(/(\d+(?:\.\d+)?)/.exec(a)?.[1]);
      return Math.abs(said - gb) <= 1 ? true : `${said} GB instead of ${gb} GB`;
    },
  },
  {
    id: "s04-processor", area: "settings",
    prompt: () => `Using the Windows Settings app, find this computer's processor. Answer with its model (for example "Core i7-12700" or "Ryzen 7 5800X").`,
    check: (_c, a) => {
      const name = ps("(Get-CimInstance Win32_Processor | Select-Object -First 1).Name");
      const model = /(i\d-\s?\w+|Ryzen \d \w+|Core \w+ \w+|Snapdragon[\w ]+)/i.exec(name)?.[1] ?? name;
      return has(a.replace(/-/g, " "), model.replace(/-/g, " "));
    },
  },
  {
    id: "s05-time-zone", area: "settings",
    prompt: () => `Using the Windows Settings app, find which time zone this computer is set to. Answer with the UTC offset, like UTC+01:00.`,
    check: (_c, a) => {
      const off = ps("$o=(Get-TimeZone).BaseUtcOffset; '{0}{1:00}:{2:00}' -f ($(if($o -lt 0){'-'}else{'+'})), [math]::Abs($o.Hours), [math]::Abs($o.Minutes)");
      return has(a.replace(/\s/g, ""), `UTC${off}`);
    },
  },
  {
    id: "s06-resolution", area: "settings",
    prompt: () => `Using the Windows Settings app, find the display resolution of the main display. Answer like 1920 x 1080.`,
    check: (_c, a) => {
      const r = ps(`Add-Type 'using System;using System.Runtime.InteropServices;public class R{[DllImport("user32")]public static extern bool SetProcessDpiAwarenessContext(IntPtr v);[DllImport("user32")]public static extern int GetSystemMetrics(int i);}'; [void][R]::SetProcessDpiAwarenessContext([IntPtr]-4); '{0} {1}' -f [R]::GetSystemMetrics(0),[R]::GetSystemMetrics(1)`);
      const [w, h] = r.split(" ");
      const m = /(\d{3,5})\s*[x×*]\s*(\d{3,5})/i.exec(a);
      return m && m[1] === w && m[2] === h ? true : `"${a}" instead of ${w} x ${h}`;
    },
  },
  {
    id: "s07-theme", area: "settings",
    prompt: () => `Using the Windows Settings app, find whether apps are set to light or dark mode. Answer "light" or "dark".`,
    check: (_c, a) => has(a, ps("(Get-ItemProperty HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize).AppsUseLightTheme") === "1" ? "light" : "dark"),
  },
  {
    id: "s08-language", area: "settings",
    prompt: () => `Using the Windows Settings app, find the Windows display language. Answer with the language as Settings names it.`,
    check: (_c, a) => {
      const tag = ps("(Get-WinUserLanguageList)[0].LanguageTag");
      const names = ps(`$c=[Globalization.CultureInfo]::new('${tag}'); $c.DisplayName; $c.NativeName; $c.EnglishName`).split(/\r?\n/);
      const first = (s: string) => norm(s).split(/[ (（]/)[0];
      return names.some((n) => norm(a).includes(first(n))) ? true : `"${a}" is not ${names.join(" / ")}`;
    },
  },
  {
    id: "s09-scale", area: "settings",
    prompt: () => `Using the Windows Settings app, find the display scale of the main display (in Display settings). Answer with the percentage.`,
    check: (_c, a) => {
      const pct = Number(ps("Add-Type 'using System.Runtime.InteropServices;public class Q{[DllImport(\"user32\")]public static extern uint GetDpiForSystem();[DllImport(\"user32\")]public static extern bool SetProcessDpiAwarenessContext(System.IntPtr v);}'; [void][Q]::SetProcessDpiAwarenessContext([IntPtr]-4); [math]::Round([Q]::GetDpiForSystem()*100/96)"));
      return Number(/(\d{2,3})\s*%?/.exec(a)?.[1]) === pct ? true : `"${a}" instead of ${pct}%`;
    },
  },
  {
    id: "s10-storage", area: "settings",
    prompt: () => `Using the Windows Settings app (System > Storage), find how much space is used on drive C:. Answer with the number of GB used.`,
    check: (_c, a) => {
      const used = Number(ps("$d=Get-PSDrive C; [math]::Round($d.Used/1GB)"));
      const said = Number(/(\d+(?:[.,]\d+)?)/.exec(a)?.[1]?.replace(",", "."));
      return Math.abs(said - used) <= Math.max(3, used * 0.03) ? true : `${said} GB instead of about ${used} GB`;
    },
  },

  // ---------- Office: Word, Excel, PowerPoint; checked by opening the saved file
  {
    id: "o01-word-new", area: "office",
    prompt: (c) => `In Microsoft Word, create a new document whose first line is "Quarterly Report" and second line is "Sales grew by 12 percent." ${SAVE(c, "report.docx")}.`,
    check: (c) => has(plain(part(path.join(c.work, "report.docx"), "word/document.xml")), "Quarterly Report", "Sales grew by 12 percent."),
  },
  {
    id: "o02-word-bold", area: "office",
    prompt: (c) => `In Microsoft Word, create a document with the heading "Meeting Notes" in bold, followed by a normal line "Attendees: Ana, Ben". ${SAVE(c, "notes.docx")}.`,
    check: (c) => {
      const x = part(path.join(c.work, "notes.docx"), "word/document.xml");
      const para = [...x.matchAll(/<w:p[ >][\s\S]*?<\/w:p>/g)].map((m) => m[0]).find((p) => plain(p).includes("Meeting Notes"));
      if (!para) return "no paragraph says Meeting Notes";
      return all(/<w:b\/>|<w:b w:val="(1|true)"\/>|w:val="Heading|w:val="Title/.test(para) ? true : "Meeting Notes is not bold", has(plain(x), "Attendees: Ana, Ben"));
    },
  },
  {
    id: "o03-word-table", area: "office",
    prompt: (c) => `In Microsoft Word, create a document with a table of 2 columns and 3 rows: a header row "Item" | "Cost", then "Paper" | "4", then "Ink" | "19". ${SAVE(c, "costs.docx")}.`,
    check: (c) => {
      const x = part(path.join(c.work, "costs.docx"), "word/document.xml");
      const rows = [...(/<w:tbl>[\s\S]*?<\/w:tbl>/.exec(x)?.[0] ?? "").matchAll(/<w:tr[ >][\s\S]*?<\/w:tr>/g)].map((r) => [...r[0].matchAll(/<w:tc>[\s\S]*?<\/w:tc>/g)].map((c) => plain(c[0]).trim()).join("|"));
      return rows.join(";") === "Item|Cost;Paper|4;Ink|19" ? true : `the table is ${JSON.stringify(rows)}`;
    },
  },
  {
    id: "o04-word-edit", area: "office",
    setup: (c) => writeFileSync(path.join(c.work, "letter.rtf"), "{\\rtf1\\ansi{\\fonttbl{\\f0 Calibri;}}\\f0\\fs22 Dear Sir,\\par\\par Thank you for your order of 12 chairs.\\par\\par Kind regards,\\par Kim\\par}"),
    prompt: (c) => `Open ${path.join(c.work, "letter.rtf")} in Microsoft Word, change "Dear Sir," to "Dear Ms. Lee," and "12 chairs" to "14 chairs", and save it as a Word document: ${path.join(c.work, "letter.docx")}.`,
    check: (c) => { const t = plain(part(path.join(c.work, "letter.docx"), "word/document.xml")); return all(has(t, "Dear Ms. Lee,", "14 chairs", "Kind regards"), /Dear Sir|12 chairs/.test(t) ? "the old text is still there" : true); },
  },
  {
    id: "o05-excel-sum", area: "office",
    prompt: (c) => `In Microsoft Excel, create a new workbook with 10, 20, 30, 40, 50 in cells A1 to A5 and a formula in A6 that adds them up. ${SAVE(c, "sum.xlsx")}.`,
    check: (c) => {
      const x = cells(path.join(c.work, "sum.xlsx"));
      return all(eq(["A1", "A2", "A3", "A4", "A5"].map((r) => cell(x, r)).join(","), "10,20,30,40,50"), /SUM\(A1:A5\)|A1\+A2\+A3\+A4\+A5/i.test(x.get("A6")?.f ?? "") ? true : `A6 is ${JSON.stringify(x.get("A6"))}`);
    },
  },
  {
    id: "o06-excel-column", area: "office",
    setup: (c) => writeFileSync(path.join(c.work, "sales.csv"), "Region,Amount\nNorth,120\nSouth,340\nEast,95\nWest,210\n"),
    prompt: (c) => `Open ${path.join(c.work, "sales.csv")} in Microsoft Excel. Add a column C with the header "Double" whose cells are formulas giving twice the Amount of each row. Save it as an Excel workbook: ${path.join(c.work, "sales.xlsx")}.`,
    check: (c) => {
      const x = cells(path.join(c.work, "sales.xlsx"));
      return all(eq(cell(x, "C1"), "Double"), eq(["C2", "C3", "C4", "C5"].map((r) => cell(x, r)).join(","), "240,680,190,420"), x.get("C2")?.f ? true : "C2 is not a formula");
    },
  },
  {
    id: "o07-excel-sort", area: "office",
    setup: (c) => writeFileSync(path.join(c.work, "scores.csv"), "Name,Score\nAna,71\nBen,93\nCal,58\nDee,88\nEve,64\n"),
    prompt: (c) => `Open ${path.join(c.work, "scores.csv")} in Microsoft Excel, sort the rows by Score from highest to lowest (keeping the header on top), and save it as ${path.join(c.work, "scores.xlsx")}.`,
    check: (c) => { const x = cells(path.join(c.work, "scores.xlsx")); return eq([2, 3, 4, 5, 6].map((r) => cell(x, `A${r}`)).join(","), "Ben,Dee,Ana,Eve,Cal"); },
  },
  {
    id: "o08-excel-sheet", area: "office",
    prompt: (c) => `In Microsoft Excel, create a new workbook, rename its sheet to "Budget", and put "Rent" in A1 and 1200 in B1. ${SAVE(c, "budget.xlsx")}.`,
    check: (c) => {
      const f = path.join(c.work, "budget.xlsx");
      const x = cells(f);
      return all(/<sheet [^>]*name="Budget"/.test(part(f, "xl/workbook.xml")) ? true : "the sheet is not named Budget", eq(cell(x, "A1"), "Rent"), eq(cell(x, "B1"), "1200"));
    },
  },
  {
    id: "o09-ppt-slides", area: "office",
    prompt: (c) => `In Microsoft PowerPoint, create a presentation with 3 slides titled "Welcome", "Plan" and "Questions", in that order. ${SAVE(c, "deck.pptx")}.`,
    check: (c) => {
      const f = path.join(c.work, "deck.pptx");
      if (!existsSync(f)) return "deck.pptx was not saved";
      const titles = [1, 2, 3].map((n) => { try { return plain(part(f, `ppt/slides/slide${n}.xml`)).trim(); } catch { return ""; } });
      return all(has(titles[0], "Welcome"), has(titles[1], "Plan"), has(titles[2], "Questions"));
    },
  },
  {
    id: "o10-ppt-bullets", area: "office",
    prompt: (c) => `In Microsoft PowerPoint, create a presentation with one slide titled "Goals" with three bullet points: "Ship v2", "Hire two engineers", "Cut costs". ${SAVE(c, "goals.pptx")}.`,
    check: (c) => has(plain(part(path.join(c.work, "goals.pptx"), "ppt/slides/slide1.xml").replace(/<\/a:p>/g, "\n")), "Goals", "Ship v2", "Hire two engineers", "Cut costs"),
  },

  // ---------- VS Code: a project folder of its own per task
  {
    id: "v01-new-file", area: "vscode",
    prompt: (c) => `Open the folder ${c.work} in Visual Studio Code, create a file named hello.py in it containing the single line print("hi"), and save it.`,
    check: (c) => eq(read(path.join(c.work, "hello.py")).trim(), 'print("hi")'),
  },
  {
    id: "v02-replace", area: "vscode",
    setup: (c) => writeFileSync(path.join(c.work, "app.js"), "let foo = 1;\nfunction inc(foo) {\n  return foo + 1;\n}\nconsole.log(inc(foo));\n"),
    prompt: (c) => `In Visual Studio Code, open ${path.join(c.work, "app.js")}, replace every "foo" with "count", and save.`,
    check: (c) => { const t = read(path.join(c.work, "app.js")); return all(/foo/.test(t) ? "foo is still there" : true, (t.match(/count/g) ?? []).length === 4 ? true : "count does not appear 4 times"); },
  },
  {
    id: "v03-tab-size", area: "vscode",
    setup: (c) => writeFileSync(path.join(c.work, "main.ts"), "export const x = 1;\n"),
    prompt: (c) => `Open the folder ${c.work} in Visual Studio Code and, in the Settings editor, set Tab Size to 2 for this workspace only (the Workspace tab, not User).`,
    check: (c) => { const f = path.join(c.work, ".vscode", "settings.json"); return /"editor\.tabSize"\s*:\s*2\b/.test(read(f)) ? true : "the workspace settings have no tabSize 2"; },
  },
  {
    id: "v04-sort-lines", area: "vscode",
    setup: (c) => writeFileSync(path.join(c.work, "names.txt"), "Mona\nAbe\nZed\nLou\nCarl\n"),
    prompt: (c) => `In Visual Studio Code, open ${path.join(c.work, "names.txt")}, sort its lines in ascending order, and save.`,
    check: (c) => eq(read(path.join(c.work, "names.txt")).trim().split(/\r?\n/).join(","), "Abe,Carl,Lou,Mona,Zed"),
  },
  {
    id: "v05-search", area: "vscode",
    setup: (c) => { for (const [f, n] of [["a.ts", 3], ["b.ts", 1], ["c.md", 2]] as const) writeFileSync(path.join(c.work, f), Array.from({ length: 8 }, (_, i) => (i < n ? `// TODO: item ${i}` : `const v${i} = ${i};`)).join("\n")); },
    prompt: (c) => `Open the folder ${c.work} in Visual Studio Code and use its search to count how many times "TODO" appears across all files. Answer with the number.`,
    check: (_c, a) => Number(/\d+/.exec(a)?.[0]) === 6 ? true : `"${a}" instead of 6`,
  },
  {
    id: "v06-new-folder", area: "vscode",
    prompt: (c) => `Open the folder ${c.work} in Visual Studio Code. In it, create a folder named src containing a file index.ts with the line export default 42; and save it.`,
    check: (c) => eq(read(path.join(c.work, "src", "index.ts")).trim(), "export default 42;"),
  },
  {
    id: "v07-go-to-line", area: "vscode",
    setup: (c) => writeFileSync(path.join(c.work, "big.txt"), Array.from({ length: 120 }, (_, i) => `line ${i + 1}: ${((i + 1) * 7919 % 1000).toString(36)}`).join("\n")),
    prompt: (c) => `In Visual Studio Code, open ${path.join(c.work, "big.txt")} and go to line 87. What does that line say? Answer with the whole line.`,
    check: (_c, a) => has(a, `line 87: ${(87 * 7919 % 1000).toString(36)}`),
  },
  {
    id: "v08-delete-lines", area: "vscode",
    setup: (c) => writeFileSync(path.join(c.work, "run.log"), ["INFO start", "DEBUG x=1", "INFO step 1", "DEBUG y=2", "WARN slow", "DEBUG z=3", "INFO done"].join("\n") + "\n"),
    prompt: (c) => `In Visual Studio Code, open ${path.join(c.work, "run.log")}, delete every line that contains DEBUG, and save.`,
    check: (c) => eq(read(path.join(c.work, "run.log")).trim().split(/\r?\n/).join(","), "INFO start,INFO step 1,WARN slow,INFO done"),
  },
  {
    id: "v09-line-endings", area: "vscode",
    setup: (c) => writeFileSync(path.join(c.work, "unix.sh"), "#!/bin/sh\r\necho one\r\necho two\r\n"),
    prompt: (c) => `In Visual Studio Code, open ${path.join(c.work, "unix.sh")}, change its line endings from CRLF to LF, and save.`,
    check: (c) => { const t = read(path.join(c.work, "unix.sh")); return all(t.includes("\r") ? "it still has CRLF" : true, has(t, "echo two")); },
  },
  {
    id: "v10-format-json", area: "vscode",
    setup: (c) => writeFileSync(path.join(c.work, "data.json"), '{"name":"p0","tags":["a","b"],"size":{"w":3,"h":4}}'),
    prompt: (c) => `In Visual Studio Code, open ${path.join(c.work, "data.json")}, format the document (so each property is on its own line), and save.`,
    check: (c) => {
      const t = read(path.join(c.work, "data.json"));
      try { JSON.parse(t); } catch { return "it is no longer valid JSON"; }
      return t.split(/\r?\n/).length >= 8 ? true : "it is not formatted";
    },
  },
];

export function prepare(t: Task, c: Ctx) {
  mkdirSync(c.work, { recursive: true });
  t.setup?.(c);
}
