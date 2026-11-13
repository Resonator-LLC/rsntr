// How the last-modified time of a digest is shown: the time for today (or
// the last six hours), the date otherwise.
//
// Port of `org.waveprotocol.wave.client.common.util.DateUtils.formatPastDate`
// of Apache Wave (Apache License 2.0) with the formats GWT's English
// locale gives it, whatever the browser's locale: short time `h:mm a` in
// lower case, `MONTH_ABBR_DAY` `MMM d`, medium date `MMM d, y`. Times are in
// the browser's time zone.

const MIN_MS = 60 * 1000;
const HOUR_MS = 60 * MIN_MS;
const DAY_MS = 24 * HOUR_MS;

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** `formatTime`: `3:07 pm`. */
export function formatTime(date: Date): string {
  const hours = date.getHours();
  const minutes = date.getMinutes();
  const h = hours % 12 === 0 ? 12 : hours % 12;
  return `${h}:${minutes < 10 ? "0" : ""}${minutes} ${hours < 12 ? "am" : "pm"}`;
}

/** `MMM d`: `Nov 3`. */
export function formatMonthDay(date: Date): string {
  return `${MONTHS[date.getMonth()]} ${date.getDate()}`;
}

/** `MMM d, y`: `Nov 3, 2009`. */
export function formatMediumDate(date: Date): string {
  return `${formatMonthDay(date)}, ${date.getFullYear()}`;
}

function onSameDay(date: Date, now: Date): boolean {
  return (
    date.getDate() === now.getDate() &&
    date.getMonth() === now.getMonth() &&
    date.getFullYear() === now.getFullYear()
  );
}

/**
 * Formats a time in the past by how long ago it was: within six hours or
 * on the same day the time; within thirty days the day and the time; in the
 * same year the day; otherwise the date. Times in the future are formatted
 * all the same, as in the original.
 */
export function formatPastDate(time: Date | number, now: Date | number = Date.now()): string {
  const date = new Date(time);
  const current = new Date(now);
  if (Number.isNaN(date.getTime())) {
    return "";
  }
  const age = current.getTime() - date.getTime();
  if (age < 6 * HOUR_MS || onSameDay(date, current)) {
    return formatTime(date);
  } else if (age < 30 * DAY_MS) {
    return `${formatMonthDay(date)} ${formatTime(date)}`;
  } else if (date.getFullYear() === current.getFullYear()) {
    return formatMonthDay(date);
  }
  return formatMediumDate(date);
}

/** The date and time in full, for tooltips and `<time datetime>`. */
export function formatFullDateTime(time: Date | number): string {
  const date = new Date(time);
  return Number.isNaN(date.getTime()) ? "" : `${formatMediumDate(date)} ${formatTime(date)}`;
}
