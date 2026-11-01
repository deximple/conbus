// The waitlist worker. Static pages come from the assets binding; this
// worker only answers /api/*. It stores the UTC day of each write and
// nothing else about when or from where it arrived.

interface Env {
  DB: D1Database;
  ASSETS?: Fetcher;
  CONTACT_EMAIL: string;
}

const TEAM_VARIANTS = ["m8", "m15", "o249"] as const;
const TEAM_SIZES = ["1", "2-9", "10-50", "51-200", "201+"] as const;
const VENDORS = ["claude_code", "codex", "cursor", "copilot", "gemini", "other"] as const;
const UNATTENDED = ["yes", "sometimes", "no"] as const;
const SOURCES = ["pricing", "hero"] as const;
const INCIDENT_MAX = 2000;
const EMAIL_MAX = 320;
const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

function utcDay(): string {
  return new Date().toISOString().slice(0, 10);
}

function json(body: unknown, status = 200): Response {
  return Response.json(body, { status });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

async function body(request: Request): Promise<Record<string, unknown> | null> {
  try {
    const parsed: unknown = await request.json();
    return isRecord(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

async function click(request: Request, env: Env): Promise<Response> {
  const data = await body(request);
  const variant = data?.variant;
  if (
    typeof variant !== "string" ||
    !(TEAM_VARIANTS as readonly string[]).includes(variant)
  ) {
    return json({ ok: false, error: "unknown variant" }, 400);
  }
  await env.DB.prepare(
    `INSERT INTO clicks (day, variant, count) VALUES (?, ?, 1)
     ON CONFLICT (day, variant) DO UPDATE SET count = count + 1`
  )
    .bind(utcDay(), variant)
    .run();
  return json({ ok: true });
}

function invalid(error: string): Response {
  return json({ ok: false, error }, 400);
}

async function waitlist(request: Request, env: Env): Promise<Response> {
  const data = await body(request);
  if (!data) {
    return invalid("invalid request");
  }

  // Honeypot: a real person's browser never fills this. Answer as if the
  // signup landed and store nothing — a bot that filled it learns nothing.
  const website = data.website;
  if (typeof website === "string" && website !== "") {
    return json({ ok: true });
  }

  const email = data.email;
  if (typeof email !== "string" || email.length > EMAIL_MAX || !EMAIL_RE.test(email)) {
    return invalid("a valid email is required");
  }
  const teamSize = data.team_size;
  if (
    typeof teamSize !== "string" ||
    !(TEAM_SIZES as readonly string[]).includes(teamSize)
  ) {
    return invalid("choose a team size");
  }
  const vendors = data.vendors;
  if (
    !Array.isArray(vendors) ||
    vendors.some(
      (v) => typeof v !== "string" || !(VENDORS as readonly string[]).includes(v)
    )
  ) {
    return invalid("choose from the listed vendors");
  }
  const unattended = data.unattended;
  if (
    typeof unattended !== "string" ||
    !(UNATTENDED as readonly string[]).includes(unattended)
  ) {
    return invalid("choose one");
  }
  const incident = data.incident;
  if (
    incident !== undefined &&
    incident !== null &&
    (typeof incident !== "string" || incident.length > INCIDENT_MAX)
  ) {
    return invalid("the incident note is too long");
  }
  const variant = data.variant;
  if (
    variant !== undefined &&
    variant !== null &&
    variant !== "" &&
    (typeof variant !== "string" ||
      !(TEAM_VARIANTS as readonly string[]).includes(variant))
  ) {
    return invalid("unknown variant");
  }
  const source = data.source;
  if (
    typeof source !== "string" ||
    !(SOURCES as readonly string[]).includes(source)
  ) {
    return invalid("invalid request");
  }
  if (data.consent !== true) {
    return invalid("consent is required to join the waitlist");
  }

  // INSERT OR IGNORE: a duplicate email is a successful signup, not an
  // answer about who is already on the list.
  const result = await env.DB.prepare(
    `INSERT OR IGNORE INTO signups
       (day, email, team_size, vendors, unattended, incident, variant, source, consent)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`
  )
    .bind(
      utcDay(),
      email,
      teamSize,
      JSON.stringify(vendors),
      unattended,
      typeof incident === "string" ? incident : null,
      typeof variant === "string" && variant !== "" ? variant : null,
      source,
      1
    )
    .run();

  if (!result.success) {
    return json({ ok: false, error: "something went wrong" }, 500);
  }
  return json({ ok: true });
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (request.method === "POST" && url.pathname === "/api/click") {
      return click(request, env);
    }
    if (request.method === "POST" && url.pathname === "/api/waitlist") {
      return waitlist(request, env);
    }
    if (request.method === "GET" && url.pathname === "/api/contact") {
      return json({ email: env.CONTACT_EMAIL });
    }
    if (env.ASSETS) {
      return env.ASSETS.fetch(request);
    }
    return new Response("not found", { status: 404 });
  },
} satisfies ExportedHandler<Env>;
