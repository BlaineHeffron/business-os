export interface HelpFrontmatter {
  title: string;
  keywords: string[];
  order: number;
}

export interface HelpArticle {
  id: string;
  frontmatter: HelpFrontmatter;
  body: string;
}

export const PENDING_HELP_SECTION_IDS = [
  "home",
  "calls",
  "settings-google",
  "settings-dashboard",
  "settings-hubspot-deals",
  "settings-inbox",
  "settings-calls",
  "settings-system",
] as const;

export function parseHelpArticle(id: string, raw: string): HelpArticle {
  const normalized = raw.replace(/\r\n/g, "\n");
  if (!normalized.startsWith("---\n")) {
    throw new Error(`${id}: help article must start with frontmatter`);
  }

  const end = normalized.indexOf("\n---\n", 4);
  if (end === -1) {
    throw new Error(`${id}: help article frontmatter must close with ---`);
  }

  const frontmatterRaw = normalized.slice(4, end);
  const body = normalized.slice(end + "\n---\n".length).trim();
  if (body.length === 0) {
    throw new Error(`${id}: help article body is empty`);
  }

  const frontmatter = parseFrontmatter(id, frontmatterRaw);
  return { id, frontmatter, body };
}

function parseFrontmatter(id: string, raw: string): HelpFrontmatter {
  const allowed = new Set(["title", "keywords", "order"]);
  const parsed: Partial<HelpFrontmatter> = {};

  for (const line of raw.split("\n")) {
    if (!line.trim()) continue;
    const match = line.match(/^([a-z]+):\s*(.*)$/);
    if (!match) {
      throw new Error(`${id}: invalid frontmatter line "${line}"`);
    }
    const [, key, value] = match;
    if (!allowed.has(key)) {
      throw new Error(`${id}: unknown frontmatter key "${key}"`);
    }
    if (key === "title") {
      parsed.title = parseQuotedString(id, key, value);
    } else if (key === "keywords") {
      parsed.keywords = parseKeywords(id, value);
    } else if (key === "order") {
      parsed.order = parseOrder(id, value);
    }
  }

  if (!parsed.title) throw new Error(`${id}: missing title`);
  if (!parsed.keywords) throw new Error(`${id}: missing keywords`);
  if (parsed.order === undefined) throw new Error(`${id}: missing order`);

  return {
    title: parsed.title,
    keywords: parsed.keywords,
    order: parsed.order,
  };
}

function parseQuotedString(id: string, key: string, value: string): string {
  const trimmed = value.trim();
  if (!trimmed.startsWith('"') || !trimmed.endsWith('"')) {
    throw new Error(`${id}: ${key} must be a quoted string`);
  }
  const parsed = trimmed.slice(1, -1).trim();
  if (parsed.length === 0) {
    throw new Error(`${id}: ${key} must not be empty`);
  }
  return parsed;
}

function parseKeywords(id: string, value: string): string[] {
  const trimmed = value.trim();
  if (!trimmed.startsWith("[") || !trimmed.endsWith("]")) {
    throw new Error(`${id}: keywords must be a bracketed list`);
  }
  const inner = trimmed.slice(1, -1).trim();
  if (!inner) return [];
  return inner.split(",").map((part) => parseQuotedString(id, "keywords", part));
}

function parseOrder(id: string, value: string): number {
  if (!/^\d+$/.test(value.trim())) {
    throw new Error(`${id}: order must be a positive integer`);
  }
  const parsed = Number(value.trim());
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${id}: order must be a positive integer`);
  }
  return parsed;
}
