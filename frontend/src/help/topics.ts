import Fuse from "fuse.js";
import {
  SECTIONS,
  sectionEnabled,
  type AppTab,
  type OperatorSection,
  type SettingsSectionId,
  type SectionGroup,
} from "../lib/sections";
import { parseHelpArticle, type HelpArticle } from "./index";

export const GETTING_AROUND_TOPIC_ID = "getting-around";

export interface HelpTopic {
  id: string;
  title: string;
  keywords: string[];
  body: string;
  order: number;
  group: SectionGroup | "Help";
  section?: OperatorSection;
}

export interface HelpArticleSource {
  id: string;
  raw: string;
}

const articleModules = import.meta.glob("./*.md", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

const DEFAULT_ARTICLES: readonly HelpArticleSource[] = Object.entries(
  articleModules,
).map(([path, raw]) => ({
  id: articleIdFromPath(path),
  raw,
}));

export function defaultHelpArticles(): readonly HelpArticleSource[] {
  return DEFAULT_ARTICLES;
}

export function buildHelpTopics({
  enabledSlices,
  articles = DEFAULT_ARTICLES,
}: {
  enabledSlices: readonly string[] | null;
  articles?: readonly HelpArticleSource[];
}): HelpTopic[] {
  const sectionsById = new Map(SECTIONS.map((section) => [section.id, section]));
  const topics: HelpTopic[] = [gettingAroundTopic()];

  for (const source of articles) {
    const section = sectionsById.get(source.id);
    if (!section || !sectionEnabled(section, enabledSlices)) continue;
    const article = parseHelpArticle(source.id, source.raw);
    topics.push(topicFromArticle(article, section));
  }

  return topics.sort((a, b) => {
    if (a.id === GETTING_AROUND_TOPIC_ID) return -1;
    if (b.id === GETTING_AROUND_TOPIC_ID) return 1;
    if (a.group !== b.group) return groupOrder(a.group) - groupOrder(b.group);
    return a.order - b.order || a.title.localeCompare(b.title);
  });
}

export function searchHelpTopics(
  topics: readonly HelpTopic[],
  query: string,
): HelpTopic[] {
  const trimmed = query.trim();
  if (!trimmed) return [...topics];
  const fuse = new Fuse(topics, {
    keys: [
      { name: "title", weight: 0.55 },
      { name: "keywords", weight: 0.3 },
      { name: "body", weight: 0.15 },
    ],
    threshold: 0.35,
    ignoreLocation: true,
    includeScore: true,
    minMatchCharLength: 2,
  });
  return fuse.search(trimmed).map((result) => result.item);
}

export function articleBackedHelpTopics(
  topics: readonly HelpTopic[],
): HelpTopic[] {
  return topics.filter((topic) => topic.section !== undefined);
}

export function helpTopicIdForTab(
  topics: readonly HelpTopic[],
  tab: AppTab,
): string | undefined {
  return articleBackedHelpTopics(topics).find((topic) => topic.section?.tab === tab)
    ?.id;
}

export function helpTopicIdForSettingsSection(
  topics: readonly HelpTopic[],
  settingsSection: SettingsSectionId | null,
): string | undefined {
  if (settingsSection === null) return undefined;
  return articleBackedHelpTopics(topics).find(
    (topic) => topic.section?.settingsSection === settingsSection,
  )?.id;
}

export function helpCommandIdsForTopics(topics: readonly HelpTopic[]): string[] {
  return articleBackedHelpTopics(topics).map((topic) => `help-${topic.id}`);
}

function topicFromArticle(article: HelpArticle, section: OperatorSection): HelpTopic {
  return {
    id: article.id,
    title: article.frontmatter.title,
    keywords: article.frontmatter.keywords,
    body: article.body,
    order: article.frontmatter.order,
    group: section.group,
    section,
  };
}

function gettingAroundTopic(): HelpTopic {
  const mac =
    typeof navigator !== "undefined" && /Mac|iP/.test(navigator.platform);
  const commandKey = mac ? "Command-K" : "Ctrl-K";
  const shortcutSymbol = mac ? "⌘K" : "Ctrl+K";
  return {
    id: GETTING_AROUND_TOPIC_ID,
    title: "Getting around",
    keywords: ["shortcuts", "keyboard", "commands", "navigation"],
    order: 0,
    group: "Help",
    body: `# Getting around

## What this does

Use keyboard shortcuts when you want to move quickly without leaving the current screen.

## Shortcuts

- ${shortcutSymbol}: open the command palette.
- ?: open Help.
- Esc: close panels, popovers, and inline editors.
- j / k: move focus in Queue and Inbox.
- Enter: open or expand the focused item in Queue and Inbox.
- a: accept the focused item in Queue.
- x: dismiss the focused item in Queue.

## Common tasks

- Find an action: press ${commandKey}, type what you want to do, then press Enter.
- Review work quickly: use j and k to move through the list, then Enter to open the selected item.
- Close a panel: press Esc or click outside it.`,
  };
}

function articleIdFromPath(path: string): string {
  const match = path.match(/^\.\/(.+)\.md$/);
  if (!match) throw new Error(`unexpected help article path: ${path}`);
  return match[1];
}

function groupOrder(group: HelpTopic["group"]): number {
  switch (group) {
    case "Help":
      return 0;
    case "Work":
      return 1;
    case "Records":
      return 2;
    case "Automation":
      return 3;
    case "System":
      return 4;
  }
}
