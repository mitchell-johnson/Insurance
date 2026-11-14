// Talking to the running system from the seeding and the tests' set-up, the
// way its own services do: the portal's routes as a person or a module, the
// API, and a module's group on the ledger nodes with the cluster token.
"use strict";
const crypto = require("crypto");

class System {
  constructor(stack) { this.stack = stack; }

  async http(method, base, pathname, { headers = {}, body, raw } = {}) {
    const h = { ...headers };
    if (method !== "GET" && !h["idempotency-key"]) h["idempotency-key"] = crypto.randomUUID();
    if (body !== undefined && !raw) h["content-type"] = "application/json";
    const res = await fetch(base + pathname, { method, headers: h, body: body === undefined ? undefined : raw ? body : JSON.stringify(body) });
    const text = await res.text();
    let data; try { data = JSON.parse(text); } catch { data = text; }
    return { status: res.status, ok: res.ok, data };
  }
  /// A call to the portal as a signed-in person (development sign-in headers).
  async portal(method, pathname, who, body, key) {
    const headers = { "x-dev-subject": who.subject };
    if (key) headers["idempotency-key"] = key;
    if (who.roles) headers["x-dev-roles"] = who.roles;
    return this.http(method, this.stack.portal, pathname, { headers, body });
  }
  async must(promise, what) {
    const r = await promise;
    if (!r.ok) throw new Error(`${what}: ${r.status} ${typeof r.data === "string" ? r.data : JSON.stringify(r.data)}`);
    return r.data;
  }
  /// A channel module command from another module, through the portal's internal route.
  async internal(module, command, key) {
    return this.must(this.http("POST", this.stack.portal, "/internal/v1/commands", { headers: { "x-dev-subject": `svc-${module}`, "x-dev-roles": module, "x-module": module, ...(key ? { "idempotency-key": key } : {}) }, body: command }), `${module} -> channels ${Object.keys(command)[0]}`);
  }
  /// The core's API, as a named calling service.
  async api(method, pathname, body, caller = "new-business") {
    return this.http(method, this.stack.api, pathname, { headers: { "x-dev-subject": caller, "x-dev-roles": caller, "x-caller": caller }, body });
  }
  async node(pathname, body) {
    let last = "";
    for (let round = 0; round < 40; round++) {
      for (const node of this.stack.nodes) {
        try {
          const res = await fetch(node + pathname, { method: "POST", headers: { authorization: `Bearer ${this.stack.token}`, "content-type": "application/json" }, body: JSON.stringify(body) });
          if (res.ok) return res.json();
          last = `${res.status} ${await res.text()}`;
        } catch (e) { last = String(e); }
      }
      await new Promise((r) => setTimeout(r, 250));
    }
    throw new Error(`no ledger node took ${pathname}: ${last}`);
  }
  /// A command for a module's group, as one of that module's own services sends it.
  tell(module, envelope) { return this.node(`/v1/modules/${module}/commands`, envelope); }
  /// A question for a module's group.
  ask(module, question) { return this.node(`/v1/modules/${module}/query`, question); }
}

module.exports = { System };
