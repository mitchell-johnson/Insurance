// Shared by every page: sign-in, calls to the portal service, and small DOM helpers.
// Nothing is ever written with innerHTML: every value from the service is set as text.
"use strict";
const portal = (() => {
  let config = null;
  const store = window.sessionStorage;

  function h(tag, attrs, ...children) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (k === "on") for (const [ev, fn] of Object.entries(v)) el.addEventListener(ev, fn);
      else if (v === true) el.setAttribute(k, "");
      else if (v !== false && v != null) el.setAttribute(k, v);
    }
    for (const c of children.flat()) if (c != null) el.append(c.nodeType ? c : document.createTextNode(String(c)));
    return el;
  }
  function clear(el) { while (el.firstChild) el.removeChild(el.firstChild); return el; }
  // Replace an element's content. Unlike append, it leaves out anything null.
  function put(el, ...children) { clear(el).append(...children.flat().filter((c) => c != null)); return el; }
  function money(minor, currency) {
    const n = Number(minor) / 100;
    try { return new Intl.NumberFormat(undefined, { style: "currency", currency: currency || "NZD" }).format(n); } catch { return n.toFixed(2) + " " + (currency || ""); }
  }
  function table(headers, rows) {
    return h("div", { class: "scroll" }, h("table", {},
      h("thead", {}, h("tr", {}, headers.map((t) => h("th", { scope: "col" }, t)))),
      h("tbody", {}, rows.length ? rows.map((r) => h("tr", {}, r.map((c) => (c && c.nodeType ? h("td", {}, c) : h("td", { class: typeof c === "number" ? "num" : null }, c == null ? "" : c)))))
        : h("tr", {}, h("td", { colspan: headers.length, class: "muted" }, "Nothing to show.")))));
  }
  function say(el, kind, text) { put(el, h("div", { class: "banner " + kind, role: kind === "bad" ? "alert" : "status" }, text)); }

  async function loadConfig() {
    if (!config) config = await (await fetch("/ui/config.json")).json();
    return config;
  }
  const b64url = (bytes) => btoa(String.fromCharCode(...new Uint8Array(bytes))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

  // Sign in. Development: a subject typed by the person. Production: the
  // authorization code flow with PKCE against Microsoft Entra.
  async function signIn(realm) {
    const cfg = await loadConfig();
    if (cfg.mode === "dev") {
      const key = "dev:" + realm;
      if (!store.getItem(key)) {
        const subject = window.prompt("Development sign-in. Subject:");
        if (!subject) throw new Error("Sign-in cancelled.");
        store.setItem(key, JSON.stringify({ subject, roles: window.prompt("Roles (comma separated, optional):") || "" }));
      }
      return;
    }
    const idp = cfg[realm];
    if (store.getItem("token:" + realm)) return;
    const params = new URLSearchParams(window.location.search);
    const redirect = window.location.origin + window.location.pathname;
    const base = `${idp.authority}/${idp.tenant}/oauth2/v2.0`;
    if (params.get("code") && params.get("state") === store.getItem("pkce:state")) {
      const body = new URLSearchParams({ client_id: idp.client_id, grant_type: "authorization_code", code: params.get("code"), redirect_uri: redirect, code_verifier: store.getItem("pkce:verifier"), scope: idp.scope });
      const res = await fetch(base + "/token", { method: "POST", body });
      store.removeItem("pkce:verifier"); store.removeItem("pkce:state");
      if (!res.ok) throw new Error("Sign-in failed.");
      store.setItem("token:" + realm, (await res.json()).access_token);
      window.history.replaceState({}, "", redirect);
      return;
    }
    const verifier = b64url(crypto.getRandomValues(new Uint8Array(48)));
    const state = b64url(crypto.getRandomValues(new Uint8Array(16)));
    const challenge = b64url(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)));
    store.setItem("pkce:verifier", verifier); store.setItem("pkce:state", state);
    window.location.assign(base + "/authorize?" + new URLSearchParams({ client_id: idp.client_id, response_type: "code", redirect_uri: redirect, scope: idp.scope, state, code_challenge: challenge, code_challenge_method: "S256" }));
    await new Promise(() => {});
  }
  function signOut(realm) { store.removeItem("token:" + realm); store.removeItem("dev:" + realm); window.location.reload(); }

  async function api(realm, method, path, body) {
    const cfg = await loadConfig();
    const headers = {};
    if (cfg.mode === "dev") {
      const dev = JSON.parse(store.getItem("dev:" + realm) || "{}");
      headers["x-dev-subject"] = dev.subject || "";
      if (dev.roles) headers["x-dev-roles"] = dev.roles;
    } else headers["authorization"] = "Bearer " + store.getItem("token:" + realm);
    if (method !== "GET") headers["idempotency-key"] = crypto.randomUUID();
    // A file goes as it is; anything else as JSON.
    const raw = typeof Blob !== "undefined" && body instanceof Blob;
    if (body !== undefined) headers["content-type"] = raw ? body.type || "application/octet-stream" : "application/json";
    const res = await fetch(path, { method, headers, body: body === undefined ? undefined : raw ? body : JSON.stringify(body) });
    const data = await res.json().catch(() => null);
    if (res.status === 401 && cfg.mode !== "dev") { store.removeItem("token:" + realm); throw new Error("Your sign-in has expired. Reload the page to sign in again."); }
    if (!res.ok) { const e = new Error((data && data.detail) || "The request failed (" + res.status + ")."); e.status = res.status; e.code = data && data.title; throw e; }
    return data;
  }
  // Run an action from a button or form, showing its outcome in `out`.
  function run(out, fn) {
    return async (ev) => {
      if (ev) ev.preventDefault();
      try { const done = await fn(); if (typeof done === "string") say(out, "ok", done); }
      catch (e) { say(out, "bad", e.message); }
    };
  }
  return { h, clear, put, money, table, say, signIn, signOut, api, run, loadConfig };
})();
