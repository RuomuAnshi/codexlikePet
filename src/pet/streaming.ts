/**
 * The chat model streams a structured JSON object (`{"say":"...",...}`) token
 * by token. Extract a best-effort `say` text from the partially-arrived raw
 * response so the UI can show the pet talking while the reply is still
 * incoming. Falls back to a tolerant scan of the `"say"` field while the JSON
 * is still incomplete.
 */
export function extractSayText(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed) {
    try {
      const value = JSON.parse(trimmed);
      if (value && typeof value.say === "string") return value.say;
    } catch {
      /* JSON not complete yet */
    }
  }
  const start = raw.indexOf('"say"');
  if (start === -1) return "";
  const quoteStart = raw.indexOf('"', raw.indexOf(":", start) + 1);
  if (quoteStart === -1) return "";
  let out = "";
  let escaped = false;
  for (let index = quoteStart + 1; index < raw.length; index += 1) {
    const character = raw[index];
    if (escaped) {
      out += character;
      escaped = false;
    } else if (character === "\\") {
      escaped = true;
    } else if (character === '"') {
      break;
    } else {
      out += character;
    }
  }
  return out;
}