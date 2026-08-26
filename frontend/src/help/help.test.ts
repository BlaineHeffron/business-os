import { describe, expect, it } from "vitest";
import sliceIds from "../lib/generated/slice_ids.json";
import { SECTIONS } from "../lib/sections";
import { PENDING_HELP_SECTION_IDS, parseHelpArticle } from "./index";
import {
  buildHelpTopics,
  helpCommandIdsForTopics,
  helpTopicIdForSettingsSection,
  helpTopicIdForTab,
  searchHelpTopics,
} from "./topics";

const articleModules = import.meta.glob("./*.md", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

const articleEntries = Object.entries(articleModules).map(([path, raw]) => ({
  id: articleIdFromPath(path),
  path,
  raw,
}));

const sectionIds = SECTIONS.map((section) => section.id);
const sectionIdSet = new Set(sectionIds);
const pendingIds = new Set<string>(PENDING_HELP_SECTION_IDS);
const presentArticleIds = new Set(articleEntries.map((entry) => entry.id));
const backendSliceIds = new Set<string>(sliceIds);

const bannedProsePatterns: readonly { label: string; pattern: RegExp }[] = [
  { label: "pump", pattern: /\bpump(s|ed|ing)?\b/i },
  { label: "write gate", pattern: /\bwrite\s+gate(s)?\b/i },
  { label: "produce", pattern: /\bproduce(d|s|r|rs|ing)?\b/i },
  { label: "grounded", pattern: /\bground(ed|ing)?\b/i },
  { label: "BOS_* flag", pattern: /\bBOS_[A-Z0-9_]+\b/ },
  { label: "store_core", pattern: /\bstore_core\b/i },
  { label: "outbox", pattern: /\boutbox\b/i },
  { label: "raw slice id", pattern: rawSliceIdPattern(sliceIds) },
];

describe("help content gate", () => {
  it("keeps section ids unique", () => {
    expect(new Set(sectionIds).size).toBe(sectionIds.length);
  });

  it("keeps pending help ids tied to real sections", () => {
    const unknownPending = PENDING_HELP_SECTION_IDS.filter(
      (id) => !sectionIdSet.has(id),
    );
    expect(unknownPending).toEqual([]);
  });

  it("has help articles for every required section and no stale articles", () => {
    const requiredIds = sectionIds.filter((id) => !pendingIds.has(id));
    const missingRequired = requiredIds.filter((id) => !presentArticleIds.has(id));
    const orphanArticles = articleEntries
      .map((entry) => entry.id)
      .filter((id) => !sectionIdSet.has(id));

    expect(missingRequired).toEqual([]);
    expect(orphanArticles).toEqual([]);
  });

  it("uses only real backend slice ids in section metadata", () => {
    const unknownSlices = SECTIONS.map((section) => section.slice)
      .filter((slice): slice is string => slice !== undefined)
      .filter((slice) => !backendSliceIds.has(slice));

    expect(unknownSlices).toEqual([]);
  });

  it("parses and lints every present help article", () => {
    for (const entry of articleEntries) {
      const article = parseHelpArticle(entry.id, entry.raw);
      expect(article.frontmatter.title).toBeTruthy();
      expect(article.frontmatter.keywords).toBeInstanceOf(Array);
      expect(article.frontmatter.order).toBeGreaterThan(0);

      const prose = [
        article.frontmatter.title,
        article.frontmatter.keywords.join(" "),
        article.body,
      ].join("\n");

      for (const banned of bannedProsePatterns) {
        expect(
          banned.pattern.test(prose),
          `${entry.path} contains banned operator-help term: ${banned.label}`,
        ).toBe(false);
      }
    }
  });

  it("filters disabled-section articles before search indexing", () => {
    const topics = buildHelpTopics({ enabledSlices: ["work_queue"] });
    expect(topics.map((topic) => topic.id)).toEqual([
      "getting-around",
      "queue",
      "settings",
    ]);

    const queueResults = searchHelpTopics(topics, "Queue");
    expect(queueResults.map((topic) => topic.id)).toContain("queue");

    for (const query of ["Inbox", "triage", "AI Settings", "models"]) {
      const resultIds = searchHelpTopics(topics, query).map((topic) => topic.id);
      expect(resultIds).not.toContain("inbox");
      expect(resultIds).not.toContain("settings-ai");
    }
  });

  it("generates only enabled and authored entry points", () => {
    const topics = buildHelpTopics({ enabledSlices: ["work_queue"] });

    expect(helpCommandIdsForTopics(topics)).toEqual([
      "help-queue",
      "help-settings",
    ]);
    expect(helpTopicIdForTab(topics, "queue")).toBe("queue");
    expect(helpTopicIdForTab(topics, "inbox")).toBeUndefined();
    expect(helpTopicIdForTab(topics, "tasks")).toBeUndefined();
    expect(helpTopicIdForSettingsSection(topics, "ai")).toBeUndefined();
  });
});

function articleIdFromPath(path: string): string {
  const match = path.match(/^\.\/(.+)\.md$/);
  if (!match) throw new Error(`unexpected help article path: ${path}`);
  return match[1];
}

function rawSliceIdPattern(ids: readonly string[]): RegExp {
  const escaped = ids
    .filter((id) => id.includes("_"))
    .map((id) => id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  return new RegExp(`\\b(?:${escaped.join("|")})\\b`, "i");
}
