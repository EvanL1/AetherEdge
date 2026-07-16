export class StrictJsonError extends SyntaxError {
  constructor(code, message) {
    super(message);
    this.name = "StrictJsonError";
    this.code = code;
  }
}

export const DEFAULT_JSON_BUDGETS = Object.freeze({
  maxBytes: 262_144,
  maxDepth: 64,
  maxStringCodeUnits: 262_144,
  maxObjectMembers: 4_096,
  maxArrayItems: 4_096,
  maxNumberTokenLength: 128,
});

function fail(code, message) {
  throw new StrictJsonError(code, message);
}

function resolveBudgets(overrides = {}) {
  if (overrides === null || typeof overrides !== "object") {
    throw new TypeError("strict JSON budgets must be an object");
  }
  const budgets = { ...DEFAULT_JSON_BUDGETS, ...overrides };
  for (const [name, value] of Object.entries(budgets)) {
    if (!Number.isSafeInteger(value) || value < 1) {
      throw new TypeError(`strict JSON budget ${name} must be a positive safe integer`);
    }
  }
  return budgets;
}

function assertUtf8ByteLength(text, maximum) {
  let bytes = 0;
  for (let index = 0; index < text.length; index += 1) {
    const codeUnit = text.charCodeAt(index);
    if (codeUnit <= 0x7f) {
      bytes += 1;
    } else if (codeUnit <= 0x7ff) {
      bytes += 2;
    } else if (codeUnit >= 0xd800 && codeUnit <= 0xdbff) {
      bytes += 4;
      index += 1;
    } else {
      bytes += 3;
    }
    if (bytes > maximum) {
      fail("FIELD_BOUND", `raw JSON exceeds the ${String(maximum)} byte budget`);
    }
  }
}

function decodeUtf8(input, maximumBytes) {
  if (typeof input === "string") {
    return input;
  }

  let bytes;
  if (input instanceof ArrayBuffer) {
    bytes = new Uint8Array(input);
  } else if (ArrayBuffer.isView(input)) {
    bytes = new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
  } else {
    throw new TypeError("strict JSON input must be a string, ArrayBuffer, or byte view");
  }

  if (bytes.byteLength > maximumBytes) {
    fail("FIELD_BOUND", `raw JSON exceeds the ${String(maximumBytes)} byte budget`);
  }

  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    fail("JSON_INVALID_UNICODE", "raw JSON is not well-formed UTF-8");
  }
}

function assertValidUtf16(text) {
  for (let index = 0; index < text.length; index += 1) {
    const codeUnit = text.charCodeAt(index);
    if (codeUnit >= 0xd800 && codeUnit <= 0xdbff) {
      const next = text.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) {
        fail("JSON_INVALID_UNICODE", "raw JSON contains an unpaired high surrogate");
      }
      index += 1;
    } else if (codeUnit >= 0xdc00 && codeUnit <= 0xdfff) {
      fail("JSON_INVALID_UNICODE", "raw JSON contains an unpaired low surrogate");
    }
  }
}

class JsonParser {
  constructor(text, budgets) {
    this.text = text;
    this.index = 0;
    this.budgets = budgets;
  }

  parse() {
    this.skipWhitespace();
    const value = this.parseValue(0);
    this.skipWhitespace();
    if (this.index !== this.text.length) {
      this.syntax("unexpected content after the JSON value");
    }
    return value;
  }

  syntax(message) {
    const error = new StrictJsonError(
      "JSON_SYNTAX_ERROR",
      `${message} at offset ${String(this.index)}`,
    );
    error.offset = this.index;
    throw error;
  }

  skipWhitespace() {
    while (
      this.text[this.index] === " " ||
      this.text[this.index] === "\t" ||
      this.text[this.index] === "\n" ||
      this.text[this.index] === "\r"
    ) {
      this.index += 1;
    }
  }

  parseValue(depth) {
    const character = this.text[this.index];
    switch (character) {
      case "{":
        return this.parseObject(depth + 1);
      case "[":
        return this.parseArray(depth + 1);
      case '"':
        return this.parseString();
      case "t":
        return this.parseKeyword("true", true);
      case "f":
        return this.parseKeyword("false", false);
      case "n":
        return this.parseKeyword("null", null);
      default:
        if (character === "-" || (character >= "0" && character <= "9")) {
          return this.parseNumber();
        }
        this.syntax("expected a JSON value");
    }
  }

  parseKeyword(keyword, value) {
    if (this.text.slice(this.index, this.index + keyword.length) !== keyword) {
      this.syntax(`expected ${keyword}`);
    }
    this.index += keyword.length;
    return value;
  }

  parseObject(depth) {
    if (depth > this.budgets.maxDepth) {
      fail("FIELD_BOUND", `raw JSON exceeds depth ${String(this.budgets.maxDepth)}`);
    }
    this.index += 1;
    this.skipWhitespace();
    const entries = [];
    const keys = new Set();

    if (this.text[this.index] === "}") {
      this.index += 1;
      return {};
    }

    while (this.index < this.text.length) {
      if (entries.length >= this.budgets.maxObjectMembers) {
        fail(
          "FIELD_BOUND",
          `raw JSON object exceeds ${String(this.budgets.maxObjectMembers)} members`,
        );
      }
      if (this.text[this.index] !== '"') {
        this.syntax("expected an object member name");
      }
      const key = this.parseString();
      if (keys.has(key)) {
        fail("DUPLICATE_JSON_KEY", `raw JSON object repeats member ${JSON.stringify(key)}`);
      }
      keys.add(key);

      this.skipWhitespace();
      if (this.text[this.index] !== ":") {
        this.syntax("expected ':' after an object member name");
      }
      this.index += 1;
      this.skipWhitespace();
      entries.push([key, this.parseValue(depth)]);
      this.skipWhitespace();

      const separator = this.text[this.index];
      if (separator === "}") {
        this.index += 1;
        return Object.fromEntries(entries);
      }
      if (separator !== ",") {
        this.syntax("expected ',' or '}' in an object");
      }
      this.index += 1;
      this.skipWhitespace();
    }

    this.syntax("unterminated object");
  }

  parseArray(depth) {
    if (depth > this.budgets.maxDepth) {
      fail("FIELD_BOUND", `raw JSON exceeds depth ${String(this.budgets.maxDepth)}`);
    }
    this.index += 1;
    this.skipWhitespace();
    const values = [];

    if (this.text[this.index] === "]") {
      this.index += 1;
      return values;
    }

    while (this.index < this.text.length) {
      if (values.length >= this.budgets.maxArrayItems) {
        fail(
          "FIELD_BOUND",
          `raw JSON array exceeds ${String(this.budgets.maxArrayItems)} items`,
        );
      }
      values.push(this.parseValue(depth));
      this.skipWhitespace();

      const separator = this.text[this.index];
      if (separator === "]") {
        this.index += 1;
        return values;
      }
      if (separator !== ",") {
        this.syntax("expected ',' or ']' in an array");
      }
      this.index += 1;
      this.skipWhitespace();
    }

    this.syntax("unterminated array");
  }

  parseString() {
    this.index += 1;
    let value = "";

    while (this.index < this.text.length) {
      const character = this.text[this.index];
      if (character === '"') {
        this.index += 1;
        return value;
      }
      if (character === "\\") {
        const escaped = this.parseEscape();
        if (value.length + escaped.length > this.budgets.maxStringCodeUnits) {
          fail(
            "FIELD_BOUND",
            `JSON string exceeds ${String(this.budgets.maxStringCodeUnits)} code units`,
          );
        }
        value += escaped;
        continue;
      }

      const codeUnit = this.text.charCodeAt(this.index);
      if (codeUnit <= 0x1f) {
        this.syntax("unescaped control character in a string");
      }
      if (value.length + 1 > this.budgets.maxStringCodeUnits) {
        fail(
          "FIELD_BOUND",
          `JSON string exceeds ${String(this.budgets.maxStringCodeUnits)} code units`,
        );
      }
      value += character;
      this.index += 1;
    }

    this.syntax("unterminated string");
  }

  parseEscape() {
    this.index += 1;
    const escape = this.text[this.index];
    const simple = {
      '"': '"',
      "\\": "\\",
      "/": "/",
      b: "\b",
      f: "\f",
      n: "\n",
      r: "\r",
      t: "\t",
    };
    if (Object.hasOwn(simple, escape)) {
      this.index += 1;
      return simple[escape];
    }
    if (escape !== "u") {
      this.syntax("invalid JSON string escape");
    }

    const first = this.parseHexEscape();
    if (first >= 0xd800 && first <= 0xdbff) {
      if (this.text.slice(this.index, this.index + 2) !== "\\u") {
        fail("JSON_INVALID_UNICODE", "JSON string escape contains an unpaired high surrogate");
      }
      this.index += 1;
      const second = this.parseHexEscape();
      if (!(second >= 0xdc00 && second <= 0xdfff)) {
        fail("JSON_INVALID_UNICODE", "JSON string escape contains an unpaired high surrogate");
      }
      return String.fromCodePoint(
        0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00),
      );
    }
    if (first >= 0xdc00 && first <= 0xdfff) {
      fail("JSON_INVALID_UNICODE", "JSON string escape contains an unpaired low surrogate");
    }
    return String.fromCharCode(first);
  }

  parseHexEscape() {
    if (this.text[this.index] !== "u") {
      this.syntax("expected a Unicode escape");
    }
    const hexadecimal = this.text.slice(this.index + 1, this.index + 5);
    if (!/^[0-9a-fA-F]{4}$/.test(hexadecimal)) {
      this.syntax("invalid Unicode escape");
    }
    this.index += 5;
    return Number.parseInt(hexadecimal, 16);
  }

  parseNumber() {
    const start = this.index;
    if (this.text[this.index] === "-") {
      this.index += 1;
    }

    if (this.text[this.index] === "0") {
      this.index += 1;
      if (this.isDigit(this.text[this.index])) {
        this.syntax("leading zero in a JSON number");
      }
    } else if (this.isNonzeroDigit(this.text[this.index])) {
      while (this.isDigit(this.text[this.index])) {
        this.index += 1;
      }
    } else {
      this.syntax("invalid JSON number integer part");
    }

    let hasFraction = false;
    let fractionContainsNonzero = false;
    let hasExponent = false;
    if (this.text[this.index] === ".") {
      hasFraction = true;
      this.index += 1;
      if (!this.isDigit(this.text[this.index])) {
        this.syntax("missing JSON number fraction digits");
      }
      while (this.isDigit(this.text[this.index])) {
        if (this.text[this.index] !== "0") {
          fractionContainsNonzero = true;
        }
        this.index += 1;
      }
    }

    if (this.text[this.index] === "e" || this.text[this.index] === "E") {
      hasExponent = true;
      this.index += 1;
      if (this.text[this.index] === "+" || this.text[this.index] === "-") {
        this.index += 1;
      }
      if (!this.isDigit(this.text[this.index])) {
        this.syntax("missing JSON number exponent digits");
      }
      while (this.isDigit(this.text[this.index])) {
        this.index += 1;
      }
    }

    const tokenLength = this.index - start;
    if (tokenLength > this.budgets.maxNumberTokenLength) {
      fail(
        "FIELD_BOUND",
        `JSON number exceeds ${String(this.budgets.maxNumberTokenLength)} characters`,
      );
    }
    const token = this.text.slice(start, this.index);
    const value = Number(token);
    const sourceHasIntegerSemantics =
      (!hasFraction || !fractionContainsNonzero) &&
      (!hasExponent || Number.isInteger(value));
    const unsafeFiniteInteger =
      Number.isInteger(value) && !Number.isSafeInteger(value);
    const canonicalKeepsFraction = JSON.stringify(value).includes(".");
    if (
      (sourceHasIntegerSemantics && !Number.isSafeInteger(value)) ||
      (unsafeFiniteInteger && !canonicalKeepsFraction)
    ) {
      fail("JSON_UNSAFE_NUMBER", `JSON integer number is outside the safe range: ${token}`);
    }
    if (!Number.isFinite(value)) {
      fail("JSON_NON_FINITE_NUMBER", `JSON number has a non-finite result: ${token}`);
    }
    return value;
  }

  isDigit(character) {
    return character >= "0" && character <= "9";
  }

  isNonzeroDigit(character) {
    return character >= "1" && character <= "9";
  }
}

export function preflightJson(input, budgetOverrides) {
  const budgets = resolveBudgets(budgetOverrides);
  if (typeof input === "string") {
    assertValidUtf16(input);
    assertUtf8ByteLength(input, budgets.maxBytes);
  }
  const text = decodeUtf8(input, budgets.maxBytes);
  assertValidUtf16(text);
  return new JsonParser(text, budgets).parse();
}

export function decodeJson(input, budgetOverrides) {
  return preflightJson(input, budgetOverrides);
}
