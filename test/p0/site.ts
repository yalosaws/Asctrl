// P0's local web site: small pages with real forms, so browser tasks can be checked by what the server received.
// Every form posts to /submit/<name>; the server keeps the last submission of each.

import http from "node:http";
import type { AddressInfo } from "node:net";

const page = (title: string, body: string) =>
  `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>${title} - P0 Shop</title>
<style>body{font:15px system-ui;margin:24px;max-width:760px}label{display:block;margin:8px 0}input,select,textarea{font:inherit}
table{border-collapse:collapse}td,th{border:1px solid #bbb;padding:4px 10px}.item{border:1px solid #ccc;padding:8px;margin:6px 0}
#banner{position:fixed;inset:0;background:#000a;display:flex;align-items:center;justify-content:center}#banner div{background:#fff;padding:24px}</style>
</head><body><h1>${title}</h1>${body}</body></html>`;

const PEOPLE: [string, string, number][] = [
  ["Ava Brooks", "Sales", 61200], ["Liam Chen", "Support", 48800], ["Noah Diaz", "Engineering", 93400], ["Mia Evans", "Finance", 71900],
  ["Zoe Foster", "Engineering", 98750], ["Eli Grant", "Sales", 58300], ["Ivy Hughes", "Legal", 97100], ["Owen Ito", "Support", 45200],
  ["Ruby Jones", "Design", 66400], ["Theo Khan", "Finance", 74800], ["Nora Lopez", "Design", 69900], ["Jack Moore", "Engineering", 88100],
];
const CATALOG = Array.from({ length: 25 }, (_, i) => {
  const names = ["Steel ladle", "Oak board", "Glass jar", "Linen towel", "Clay pot", "Bamboo whisk", "Iron pan", "Cork mat", "Tin box", "Wool cosy",
    "Brass bell", "Stone mortar", "Wicker tray", "Felt pad", "Copper kettle", "Ash spoon", "Jute bag", "Slate plate", "Pine crate", "Silk cloth",
    "Zinc bucket", "Teak bowl", "Rope basket", "Enamel mug", "Birch peg"];
  return { name: names[i], price: (7 + ((i * 37) % 60) + 0.99).toFixed(2) };
});
const PRODUCTS = [["Blue mug", 12], ["Red mug", 12], ["Blue plate", 18], ["Green mug", 13], ["Blue bowl", 15], ["White mug", 11]] as const;

function pages(url: URL): string | undefined {
  switch (url.pathname) {
    case "/":
      return page("Home", `<ul>${["contact", "login", "shop", "people", "catalog", "prefs", "booking", "news", "embed"].map((p) => `<li><a href="/${p}">${p}</a></li>`).join("")}</ul>`);
    case "/contact":
      return page("Contact us", `<form method="post" action="/submit/contact">
<label>Your name <input name="name"></label><label>Email <input name="email" type="email"></label>
<label>Message <textarea name="message" rows="4" cols="40"></textarea></label><button>Send message</button></form>`);
    case "/login":
      return page("Sign in", `<form method="post" action="/login"><label>Username <input name="user"></label>
<label>Password <input name="pass" type="password"></label><button>Sign in</button></form>${url.searchParams.has("bad") ? "<p>Wrong username or password.</p>" : ""}`);
    case "/account":
      return page("Your account", `<p>Signed in as demo.</p><form method="post" action="/submit/report"><input type="hidden" name="who" value="demo">
<label>Month <select name="month"><option>January</option><option>February</option><option>March</option></select></label><button>Download report</button></form>`);
    case "/shop":
      return page("Shop", `<label>Search <input id="q" oninput="for(const d of document.querySelectorAll('.item'))d.hidden=!d.dataset.n.toLowerCase().includes(this.value.toLowerCase())"></label>
${PRODUCTS.map(([n, p]) => `<div class="item" data-n="${n}"><b>${n}</b> $${p} <label>Qty <input type="number" min="1" value="1" size="3"></label>
<button onclick="add('${n}',this.previousElementSibling.firstElementChild.value)">Add to cart</button></div>`).join("")}
<h2>Cart</h2><ul id="cart"></ul><form method="post" action="/submit/order"><input type="hidden" name="cart" id="c">
<label>Ship to name <input name="name"></label><button>Checkout</button></form>
<script>const cart={};function add(n,q){cart[n]=(cart[n]||0)+Number(q);document.getElementById('cart').innerHTML=Object.entries(cart).map(([k,v])=>'<li>'+v+' x '+k+'</li>').join('');document.getElementById('c').value=JSON.stringify(cart)}</script>`);
    case "/people":
      return page("Staff directory", `<table><tr><th>Name</th><th>Team</th><th>Salary</th></tr>${PEOPLE.map(([n, t, s]) => `<tr><td>${n}</td><td>${t}</td><td>$${s.toLocaleString("en-US")}</td></tr>`).join("")}</table>`);
    case "/catalog": {
      const p = Math.min(5, Math.max(1, Number(url.searchParams.get("page") ?? 1)));
      return page(`Catalog, page ${p} of 5`, `${CATALOG.slice((p - 1) * 5, p * 5).map((c) => `<div class="item">${c.name}: $${c.price}</div>`).join("")}
<p>${p > 1 ? `<a href="/catalog?page=${p - 1}">Previous</a> ` : ""}${p < 5 ? `<a href="/catalog?page=${p + 1}">Next</a>` : ""}</p>`);
    }
    case "/prefs":
      return page("Preferences", `<form method="post" action="/submit/prefs">
<label>Language <select name="lang"><option>English</option><option>Deutsch</option><option>Français</option><option>日本語</option></select></label>
<fieldset><legend>Newsletter</legend><label><input type="checkbox" name="news" value="weekly"> Weekly digest</label>
<label><input type="checkbox" name="news" value="offers" checked> Special offers</label></fieldset>
<fieldset><legend>Theme</legend><label><input type="radio" name="theme" value="light" checked> Light</label><label><input type="radio" name="theme" value="dark"> Dark</label></fieldset>
<button>Save preferences</button></form>`);
    case "/booking":
      return page("Book a table", `<form method="post" action="/submit/booking"><label>Date <input type="date" name="date"></label>
<label>Guests <input type="number" name="guests" min="1" max="12" value="2"></label><label>Time <select name="time"><option>18:00</option><option>19:00</option><option>20:00</option></select></label>
<button>Book</button></form>`);
    case "/news":
      return page("Newsletter", `<p>Get our news every week.</p><form method="post" action="/submit/subscribe"><label>Email <input name="email"></label><button>Subscribe</button></form>
<div id="banner"><div><p>This site uses cookies.</p><button onclick="banner.remove()">Accept all</button> <button onclick="banner.remove()">Reject non-essential</button></div></div>`);
    case "/embed":
      return page("Support", `<p>Open a ticket below.</p><iframe src="/ticket" width="600" height="260" title="Ticket form"></iframe>`);
    case "/ticket":
      return `<!doctype html><html><body style="font:15px system-ui"><form method="post" action="/submit/ticket"><label>Subject <input name="subject"></label><br><br>
<label>Priority <select name="priority"><option>Low</option><option>Normal</option><option>High</option></select></label><br><br><button>Open ticket</button></form></body></html>`;
  }
}

export interface Site { url: string; got: Map<string, URLSearchParams>; close(): void }

export async function site(): Promise<Site> {
  const got = new Map<string, URLSearchParams>();
  const server = http.createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://x");
    if (req.method === "POST") {
      let body = "";
      req.on("data", (d) => (body += d));
      req.on("end", () => {
        const form = new URLSearchParams(body);
        if (url.pathname === "/login") {
          const ok = form.get("user") === "demo" && form.get("pass") === "p0-test-pass";
          res.writeHead(303, { location: ok ? "/account" : "/login?bad" }).end();
          return;
        }
        got.set(url.pathname.replace("/submit/", ""), form);
        res.writeHead(200, { "content-type": "text/html; charset=utf-8" }).end(page("Thank you", "<p>Received.</p>"));
      });
      return;
    }
    const html = pages(url);
    res.writeHead(html ? 200 : 404, { "content-type": "text/html; charset=utf-8" }).end(html ?? "Not found");
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  return { url: `http://127.0.0.1:${(server.address() as AddressInfo).port}`, got, close: () => server.close() };
}

export const answers = {
  topSalary: PEOPLE.reduce((a, b) => (b[2] > a[2] ? b : a))[0],
  kettle: CATALOG.find((c) => c.name === "Copper kettle")!.price,
  kettlePage: Math.floor(CATALOG.findIndex((c) => c.name === "Copper kettle") / 5) + 1,
};
