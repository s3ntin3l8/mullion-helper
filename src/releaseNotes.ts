export interface NotesSection { heading: string | null; items: string[]; }

// Release Please groups commits as `### Heading` + `* text ([#N](url)) ([sha](url))`.
// The webview has no opener plugin, so links would navigate the app window: drop
// them and keep the text. Output is rendered as React text, never as HTML.
function plain(text: string): string {
  return text
    .replace(/\s*\(\[[^\]]*\]\([^)]*\)\)/g, "")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\*\*/g, "")
    .trim();
}

export function parseReleaseNotes(body: string | null): NotesSection[] {
  const sections: NotesSection[] = [];
  let current: NotesSection | null = null;
  const add = (item: string) => {
    if (!item) return;
    if (!current) sections.push((current = { heading: null, items: [] }));
    current.items.push(item);
  };
  for (const raw of (body ?? "").split(/\r?\n/)) {
    const line = raw.trim();
    if (!line) continue;
    const heading = /^#{1,6}\s+(.*)$/.exec(line);
    if (heading) {
      const text = plain(heading[1]);
      // The banner already names the version, so skip `## [x.y.z](…) (date)`.
      if (/^v?\d+\.\d+\.\d+/.test(text)) { current = null; continue; }
      sections.push((current = { heading: text, items: [] }));
      continue;
    }
    add(plain(line.replace(/^[*-]\s+/, "")));
  }
  return sections.filter((section) => section.items.length > 0);
}
