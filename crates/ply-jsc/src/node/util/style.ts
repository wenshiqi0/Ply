// util.styleText and util.stripVTControlCharacters: ANSI styles by Node's
// names, and removing escape sequences from text.

const pairs: Record<string, [number, number]> = {
  reset: [0, 0], bold: [1, 22], dim: [2, 22], italic: [3, 23], underline: [4, 24], blink: [5, 25], inverse: [7, 27],
  hidden: [8, 28], strikethrough: [9, 29], doubleunderline: [21, 24], framed: [51, 54], overlined: [53, 55],
  black: [30, 39], red: [31, 39], green: [32, 39], yellow: [33, 39], blue: [34, 39], magenta: [35, 39], cyan: [36, 39], white: [37, 39],
  bgBlack: [40, 49], bgRed: [41, 49], bgGreen: [42, 49], bgYellow: [43, 49], bgBlue: [44, 49], bgMagenta: [45, 49], bgCyan: [46, 49], bgWhite: [47, 49],
  gray: [90, 39], redBright: [91, 39], greenBright: [92, 39], yellowBright: [93, 39], blueBright: [94, 39], magentaBright: [95, 39],
  cyanBright: [96, 39], whiteBright: [97, 39], bgGray: [100, 49], bgRedBright: [101, 49], bgGreenBright: [102, 49], bgYellowBright: [103, 49],
  bgBlueBright: [104, 49], bgMagentaBright: [105, 49], bgCyanBright: [106, 49], bgWhiteBright: [107, 49],
};
for (const [alias, name] of [["grey", "gray"], ["blackBright", "gray"], ["bgGrey", "bgGray"], ["bgBlackBright", "bgGray"], ["dim", "dim"], ["faint", "dim"], ["crossedout", "strikethrough"], ["strikeThrough", "strikethrough"], ["crossedOut", "strikethrough"], ["conceal", "hidden"], ["swapColors", "inverse"], ["swapcolors", "inverse"], ["doubleUnderline", "doubleunderline"]]) {
  pairs[alias] = pairs[name];
}
export const colors: Readonly<Record<string, [number, number]>> = pairs;

type Options = { validateStream?: boolean; stream?: { isTTY?: boolean } };
/** Wraps text in the named styles; to a stream that is not a terminal, plain. */
export function styleText(format: string | readonly string[], text: string, { validateStream = true, stream = process.stdout }: Options = {}): string {
  const formats = typeof format === "string" ? [format] : format;
  for (const name of formats) {
    if (!Object.hasOwn(pairs, name) && name !== "none") {
      throw Object.assign(new TypeError(`The argument 'format' must be one of: ${Object.keys(pairs).join(", ")}. Received ${JSON.stringify(name)}`), { code: "ERR_INVALID_ARG_VALUE" });
    }
  }
  if (validateStream && !stream?.isTTY) return text;
  let left = "", right = "";
  for (const name of formats) {
    if (name === "none") continue;
    const [open, close] = pairs[name];
    left += `\u001b[${open}m`;
    right = `\u001b[${close}m` + right;
  }
  return left + text + right;
}

// The escape sequences Node's stripVTControlCharacters removes.
const ESC = String.fromCharCode(0x1b), CSI = String.fromCharCode(0x9b), BEL = String.fromCharCode(7), ST = String.fromCharCode(0x9c);
const ansi = new RegExp(`[${ESC}${CSI}][[\\]()#;?]*(?:(?:(?:(?:;[-a-zA-Z\\d\\/\\#&.:=?%@~_]+)*|[a-zA-Z\\d]+(?:;[-a-zA-Z\\d\\/\\#&.:=?%@~_]*)*)?(?:${BEL}|${ESC}\\\\|${ST}))|(?:(?:\\d{1,4}(?:;\\d{0,4})*)?[\\dA-PR-TZcf-ntqry=><~]))`, "g");
export const stripVTControlCharacters = (text: string): string => String(text).replace(ansi, "");
