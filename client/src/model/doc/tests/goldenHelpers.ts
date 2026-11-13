// Record helpers shared by the golden tests of the document layer.

/** Rust's `str::lines`. */
export function lines(text: string): string[] {
  const result = text.split("\n").map((l) => (l.endsWith("\r") ? l.slice(0, -1) : l));
  if (result[result.length - 1] === "") {
    result.pop();
  }
  return result;
}

/** The escaping of the generators' `line()`: per UTF-16 code unit. */
export function escape(field: string): string {
  let escaped = "";
  for (let i = 0; i < field.length; i++) {
    const unit = field.charCodeAt(i);
    if (unit >= 0x20 && unit < 0x7f && unit !== 0x5c) {
      escaped += field.charAt(i);
    } else {
      escaped += "\\u" + unit.toString(16).toUpperCase().padStart(4, "0");
    }
  }
  return escaped;
}

export function unescape(field: string): string {
  return field.replace(/\\u([0-9A-Fa-f]{4})/g, (_, hex: string) =>
    String.fromCharCode(Number.parseInt(hex, 16)),
  );
}
