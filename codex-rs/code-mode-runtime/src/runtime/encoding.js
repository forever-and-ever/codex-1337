// WHATWG encoding interfaces, backed by encoding_rs and V8's UTF-8 writer.
// No browser, Node, network, or filesystem capabilities are installed here.
((encode, decoder, decode64, encode64) => {
  const setBytes = Function.prototype.call.bind(Uint8Array.prototype.set);
  const dictionary = (value) => {
    if (value == null) return {};
    if (typeof value !== "object" && typeof value !== "function") {
      throw new TypeError("Options must be an object");
    }
    return value;
  };
  const bufferSource = (value) => {
    if (value !== undefined && !(value instanceof ArrayBuffer) && !ArrayBuffer.isView(value)) {
      throw new TypeError("Input must be an ArrayBuffer or ArrayBufferView");
    }
    return value;
  };
  class TextEncoder {
    #brand;
    get encoding() { this.#brand; return "utf-8"; }
    encode(input = "") {
      this.#brand;
      return encode(`${input}`)[0];
    }
    encodeInto(source, destination) {
      this.#brand;
      if (arguments.length < 2) throw new TypeError("encodeInto requires two arguments");
      source = `${source}`;
      const [bytes, read] = encode(source, destination);
      setBytes(destination, bytes);
      return {read, written: bytes.byteLength};
    }
    get [Symbol.toStringTag]() { return "TextEncoder"; }
  }
  class TextDecoder {
    #encoding;
    #fatal;
    #ignoreBOM;
    #decode;
    constructor(label = "utf-8", options = {}) {
      label = `${label}`;
      options = dictionary(options);
      this.#fatal = Boolean(options.fatal);
      this.#ignoreBOM = Boolean(options.ignoreBOM);
      [this.#encoding, this.#decode] = decoder(label, this.#fatal, this.#ignoreBOM);
    }
    get encoding() { return this.#encoding; }
    get fatal() { return this.#fatal; }
    get ignoreBOM() { return this.#ignoreBOM; }
    decode(input = undefined, options = {}) {
      const decode = this.#decode;
      input = bufferSource(input);
      const stream = Boolean(dictionary(options).stream);
      return decode(input, stream);
    }
    get [Symbol.toStringTag]() { return "TextDecoder"; }
  }
  function atob(data) {
    if (arguments.length === 0) throw new TypeError("atob requires an argument");
    return decode64(`${data}`);
  }
  function btoa(data) {
    if (arguments.length === 0) throw new TypeError("btoa requires an argument");
    return encode64(`${data}`);
  }
  for (const [name, value] of Object.entries({TextEncoder, TextDecoder, atob, btoa})) {
    Object.defineProperty(globalThis, name, {value, writable: true, configurable: true});
  }
})
