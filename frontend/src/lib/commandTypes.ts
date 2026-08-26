export interface AppCommand {
  id: string;
  group: "Navigation" | "Actions";
  label: string;
  keywords?: string;
  run: () => void;
  shortcut?: string;
}
