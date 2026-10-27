// node:util/types (RFC 0015): checks by the built-in tag a value carries.
// Async functions are lowered to generators so the kernel can track their
// continuations, which isAsyncFunction then cannot see; proxies cannot be
// told apart in JS.
import type * as nodeTypes from "node:util/types";

const tag = (value: unknown) => Object.prototype.toString.call(value).slice(8, -1);
const isObject = (value: unknown): value is object => (typeof value === "object" && value !== null) || typeof value === "function";
const tagged = (name: string) => (value: unknown) => isObject(value) && tag(value) === name;
const boxed = (name: string) => (value: unknown) => typeof value === "object" && value !== null && tag(value) === name;
const typed = (name: string) => (value: unknown) => ArrayBuffer.isView(value) && tag(value) === name;

export const isArrayBuffer = tagged("ArrayBuffer") as typeof nodeTypes.isArrayBuffer;
export const isSharedArrayBuffer = tagged("SharedArrayBuffer") as typeof nodeTypes.isSharedArrayBuffer;
export const isAnyArrayBuffer = ((value: unknown) => isArrayBuffer(value) || isSharedArrayBuffer(value)) as typeof nodeTypes.isAnyArrayBuffer;
export const isArrayBufferView = ((value: unknown) => ArrayBuffer.isView(value)) as typeof nodeTypes.isArrayBufferView;
export const isDataView = tagged("DataView") as typeof nodeTypes.isDataView;
export const isTypedArray = ((value: unknown) => ArrayBuffer.isView(value) && !isDataView(value)) as typeof nodeTypes.isTypedArray;
export const isUint8Array = typed("Uint8Array") as typeof nodeTypes.isUint8Array;
export const isUint8ClampedArray = typed("Uint8ClampedArray") as typeof nodeTypes.isUint8ClampedArray;
export const isUint16Array = typed("Uint16Array") as typeof nodeTypes.isUint16Array;
export const isUint32Array = typed("Uint32Array") as typeof nodeTypes.isUint32Array;
export const isInt8Array = typed("Int8Array") as typeof nodeTypes.isInt8Array;
export const isInt16Array = typed("Int16Array") as typeof nodeTypes.isInt16Array;
export const isInt32Array = typed("Int32Array") as typeof nodeTypes.isInt32Array;
export const isFloat32Array = typed("Float32Array") as typeof nodeTypes.isFloat32Array;
export const isFloat64Array = typed("Float64Array") as typeof nodeTypes.isFloat64Array;
export const isBigInt64Array = typed("BigInt64Array") as typeof nodeTypes.isBigInt64Array;
export const isBigUint64Array = typed("BigUint64Array") as typeof nodeTypes.isBigUint64Array;
export const isArgumentsObject = tagged("Arguments") as typeof nodeTypes.isArgumentsObject;
export const isAsyncFunction = ((value: unknown) => typeof value === "function" && tag(value) === "AsyncFunction") as typeof nodeTypes.isAsyncFunction;
export const isGeneratorFunction = ((value: unknown) => typeof value === "function" && ["GeneratorFunction", "AsyncGeneratorFunction"].includes(tag(value))) as typeof nodeTypes.isGeneratorFunction;
export const isGeneratorObject = ((value: unknown) => isObject(value) && ["Generator", "AsyncGenerator"].includes(tag(value))) as typeof nodeTypes.isGeneratorObject;
export const isDate = tagged("Date") as typeof nodeTypes.isDate;
export const isRegExp = tagged("RegExp") as typeof nodeTypes.isRegExp;
export const isMap = tagged("Map") as typeof nodeTypes.isMap;
export const isSet = tagged("Set") as typeof nodeTypes.isSet;
export const isWeakMap = tagged("WeakMap") as typeof nodeTypes.isWeakMap;
export const isWeakSet = tagged("WeakSet") as typeof nodeTypes.isWeakSet;
export const isMapIterator = tagged("Map Iterator") as typeof nodeTypes.isMapIterator;
export const isSetIterator = tagged("Set Iterator") as typeof nodeTypes.isSetIterator;
export const isPromise = tagged("Promise") as typeof nodeTypes.isPromise;
export const isNativeError = ((value: unknown) => value instanceof Error || tagged("Error")(value)) as typeof nodeTypes.isNativeError;
export const isNumberObject = boxed("Number") as typeof nodeTypes.isNumberObject;
export const isStringObject = boxed("String") as typeof nodeTypes.isStringObject;
export const isBooleanObject = boxed("Boolean") as typeof nodeTypes.isBooleanObject;
export const isBigIntObject = boxed("BigInt") as typeof nodeTypes.isBigIntObject;
export const isSymbolObject = boxed("Symbol") as typeof nodeTypes.isSymbolObject;
export const isBoxedPrimitive = ((value: unknown) => isNumberObject(value) || isStringObject(value) || isBooleanObject(value) || isBigIntObject(value) || isSymbolObject(value)) as typeof nodeTypes.isBoxedPrimitive;
export const isModuleNamespaceObject = ((value: unknown) => isObject(value) && (value as { [Symbol.toStringTag]?: unknown })[Symbol.toStringTag] === "Module") as typeof nodeTypes.isModuleNamespaceObject;
export const isProxy: typeof nodeTypes.isProxy = () => false;
export const isExternal: typeof nodeTypes.isExternal = () => false;
// node:crypto is not provided, so nothing is a key object.
export const isKeyObject = (() => false) as unknown as typeof nodeTypes.isKeyObject;
export const isCryptoKey = (() => false) as unknown as typeof nodeTypes.isCryptoKey;
