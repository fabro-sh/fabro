// WCAG 2.x contrast math over the `#rrggbb` / `rgba(r, g, b, a)` colors the
// appearance palettes use. Translucent colors are composited over their actual
// surface first, because a value that clears the bar at full strength can fail
// at 0.5 opacity.

export type Rgb = readonly [number, number, number];

interface Rgba {
  rgb: Rgb;
  alpha: number;
}

const HEX_RE = /^#([0-9a-f]{6})$/i;
const RGB_RE = /^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*([\d.]+)\s*)?\)$/i;
const SHORT_HEX_RE = /^#([0-9a-f]{3})$/i;

function parseColor(color: string): Rgba | null {
  const trimmed = color.trim();
  const shortHex = trimmed.match(SHORT_HEX_RE)?.[1];
  if (shortHex) {
    const [r, g, b] = [...shortHex].map((digit) => parseInt(digit + digit, 16)) as [
      number,
      number,
      number,
    ];
    return { rgb: [r, g, b], alpha: 1 };
  }
  const hex = trimmed.match(HEX_RE)?.[1];
  if (hex) {
    return {
      rgb: [
        parseInt(hex.slice(0, 2), 16),
        parseInt(hex.slice(2, 4), 16),
        parseInt(hex.slice(4, 6), 16),
      ],
      alpha: 1,
    };
  }
  const rgb = trimmed.match(RGB_RE);
  if (rgb) {
    return {
      rgb: [Number(rgb[1]), Number(rgb[2]), Number(rgb[3])],
      alpha: rgb[4] === undefined ? 1 : Number(rgb[4]),
    };
  }
  return null;
}

function isMeasurableColor(color: string): boolean {
  return parseColor(color) !== null;
}

function requireColor(color: string): Rgba {
  const parsed = parseColor(color);
  if (!parsed) throw new Error(`Unsupported color: ${color}`);
  return parsed;
}

/** Composites `color` over an opaque `surface` color. */
export function compositeOver(color: string, surface: string): Rgb {
  const { rgb, alpha } = requireColor(color);
  const base = requireColor(surface).rgb;
  return [
    rgb[0] * alpha + base[0] * (1 - alpha),
    rgb[1] * alpha + base[1] * (1 - alpha),
    rgb[2] * alpha + base[2] * (1 - alpha),
  ];
}

/** `share` of `second` mixed into `first`, as an opaque hex color. */
export function mixColors(first: string, second: string, share: number): string {
  const a = requireColor(first).rgb;
  const b = requireColor(second).rgb;
  const channel = (index: 0 | 1 | 2) =>
    Math.round(a[index] * (1 - share) + b[index] * share)
      .toString(16)
      .padStart(2, "0");
  return `#${channel(0)}${channel(1)}${channel(2)}`;
}

function linearChannel(channel: number): number {
  const value = channel / 255;
  return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
}

function luminance(rgb: Rgb): number {
  return (
    0.2126 * linearChannel(rgb[0]) +
    0.7152 * linearChannel(rgb[1]) +
    0.0722 * linearChannel(rgb[2])
  );
}

function ratio(a: Rgb, b: Rgb): number {
  const lighter = Math.max(luminance(a), luminance(b));
  const darker = Math.min(luminance(a), luminance(b));
  return (lighter + 0.05) / (darker + 0.05);
}

function rgbString(rgb: Rgb): string {
  return `rgb(${rgb.map((channel) => Math.round(channel)).join(", ")})`;
}

/**
 * WCAG contrast of `foreground` against `background`. A translucent background
 * is composited over `base` (required when the background has any transparency),
 * and a translucent foreground over the resulting surface.
 */
export function contrastRatio(
  foreground: string,
  background: string,
  base?: string,
): number {
  const surface = base
    ? compositeOver(background, base)
    : compositeOver(background, background);
  return ratio(compositeOver(foreground, rgbString(surface)), surface);
}

/**
 * Picks whichever of the two inks reads better on an arbitrary opaque fill, for
 * surfaces Fabro does not control such as a workflow author's `fillcolor`.
 * Returns null when the fill can't be measured (named colors, gradients).
 */
export function readableInkOn<T extends string>(
  fill: string,
  darkInk: T,
  lightInk: T,
): T | null {
  if (!isMeasurableColor(fill)) return null;
  return contrastRatio(darkInk, fill) >= contrastRatio(lightInk, fill)
    ? darkInk
    : lightInk;
}
