import { env, exports } from "cloudflare:workers";
import { describe, it, expect, beforeAll } from "vitest";
import schema from "../schema.sql?raw";

beforeAll(async () => {
  // One statement per exec call, on one line — workerd's exec treats each
  // line as a statement.
  for (const statement of schema.split(";")) {
    const sql = statement
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line !== "" && !line.startsWith("--"))
      .join(" ");
    if (sql) {
      await env.DB.exec(sql);
    }
  }
});

function post(path: string, payload: unknown) {
  return exports.default.fetch(`https://site.test${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload),
  });
}

function signup(overrides: Record<string, unknown> = {}) {
  return {
    email: "lead@example.com",
    team_size: "10-50",
    vendors: ["claude_code", "codex"],
    unattended: "sometimes",
    incident: "nightly batch died on an expired token; found it in the morning",
    variant: "m8",
    source: "pricing",
    consent: true,
    website: "",
    ...overrides,
  };
}

describe("POST /api/click", () => {
  it("upserts one row per day and variant", async () => {
    expect((await post("/api/click", { variant: "m15" })).status).toBe(200);
    expect((await post("/api/click", { variant: "m15" })).status).toBe(200);
    expect((await post("/api/click", { variant: "o249" })).status).toBe(200);

    const rows = await env.DB.prepare(
      "SELECT variant, count FROM clicks ORDER BY variant"
    ).all<{ variant: string; count: number }>();
    expect(rows.results).toEqual([
      { variant: "m15", count: 2 },
      { variant: "o249", count: 1 },
    ]);
  });

  it("rejects a variant nobody is shown", async () => {
    const res = await post("/api/click", { variant: "m99" });
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ ok: false, error: "unknown variant" });
  });
});

describe("POST /api/waitlist", () => {
  it("accepts a valid signup and stores the day, not a timestamp", async () => {
    const res = await post("/api/waitlist", signup());
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true });

    const row = await env.DB.prepare(
      "SELECT day, email, team_size, vendors, unattended, incident, variant, source, consent FROM signups WHERE email = ?"
    )
      .bind("lead@example.com")
      .first<Record<string, unknown>>();
    expect(row).toMatchObject({
      email: "lead@example.com",
      team_size: "10-50",
      vendors: '["claude_code","codex"]',
      unattended: "sometimes",
      variant: "m8",
      source: "pricing",
      consent: 1,
    });
    expect(String(row?.day)).toMatch(/^\d{4}-\d{2}-\d{2}$/);
  });

  it("rejects without consent, a bad email, or a bad enum", async () => {
    for (const bad of [
      signup({ email: "not-an-email", team_size: "10-50" }),
      signup({ email: "dup-consent@example.com", consent: false }),
      signup({ email: "dup-team@example.com", team_size: "huge" }),
      signup({ email: "dup-vendor@example.com", vendors: ["emacs"] }),
      signup({ email: "dup-unatt@example.com", unattended: "always" }),
      signup({ email: "dup-source@example.com", source: "ad" }),
      signup({ email: "dup-incident@example.com", incident: "x".repeat(2001) }),
    ]) {
      const res = await post("/api/waitlist", bad);
      expect(res.status).toBe(400);
      expect((await res.json<{ ok: boolean }>()).ok).toBe(false);
    }
    const count = await env.DB.prepare(
      "SELECT COUNT(*) AS n FROM signups WHERE email LIKE 'dup-%'"
    ).first<{ n: number }>();
    expect(count?.n).toBe(0);
  });

  it("answers a filled honeypot as if it worked and stores nothing", async () => {
    const res = await post(
      "/api/waitlist",
      signup({ email: "bot@example.com", website: "https://spam.example" })
    );
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true });
    const row = await env.DB.prepare("SELECT id FROM signups WHERE email = ?")
      .bind("bot@example.com")
      .first();
    expect(row).toBeNull();
  });

  it("answers a duplicate email the same as a fresh signup", async () => {
    expect((await post("/api/waitlist", signup())).status).toBe(200);
    const res = await post("/api/waitlist", signup({ team_size: "201+" }));
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true });
    const count = await env.DB.prepare(
      "SELECT COUNT(*) AS n FROM signups WHERE email = ?"
    )
      .bind("lead@example.com")
      .first<{ n: number }>();
    expect(count?.n).toBe(1);
  });
});
