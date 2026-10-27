// Promise scheduling for call ownership. Helpers below are from
// @oxc-project/runtime 0.155.0 (MIT); see vendor/oxc-async.LICENSE.
// Native await bypasses Promise.prototype.then, so every evaluated module,
// prelude and model bundle is lowered by OXC before entering JSC.
(function (hostBind, registerCancel, unregisterCancel, currentCall, callCancelled) {
  "use strict";
  const NativePromise = globalThis.Promise;
  const nativeThen = NativePromise.prototype.then;
  const nativeResolve = NativePromise.resolve.bind(NativePromise);
  const apply = Reflect.apply;
  const owners = new WeakMap();
  let unhandled = () => {};
  // Promises that settle through their own chain (`then` results, async
  // function results) are rejected only when a cancelled call is retired;
  // what a cancelled call awaits is interrupted at once instead.
  let deriving = false;
  const derived = executor => { deriving = true; return new Promise(executor); };
  // Which call created a promise. A cancelled call's waits on promises it
  // did not create are interrupted; its own settle through their chain.
  const creators = new WeakMap();
  // Async frames executing in this turn: { reject, observed }. A terminated
  // turn never unwinds them, so their promises are rejected for them.
  const running = [];
  const cancelledError = () => Object.assign(new Error("the call was cancelled"), { name: "AbortError", code: "ERR_PLY_CANCELLED" });
  const own = promise => {
    owners.set(promise, hostBind(reason => unhandled(reason), true));
    return promise;
  };
  const Promise = new Proxy(NativePromise, {
    construct(target, args, newTarget) {
      const executor = args[0];
      const leaf = !deriving;
      deriving = false;
      if (typeof executor !== "function") throw new TypeError("Promise resolver is not a function");
      let promise;
      promise = Reflect.construct(target, [(resolve, reject) => {
        const token = registerCancel(reject, leaf);
        let locked = false;
        const fulfilled = value => { unregisterCancel(token); resolve(value); };
        const rejected = error => { unregisterCancel(token); reject(error); };
        const accept = value => {
          if (locked) return;
          locked = true;
          if (value !== null && (typeof value === "object" || typeof value === "function")) {
            if (value === promise) { rejected(new TypeError("Cannot resolve a promise with itself")); return; }
            // Keep the outer Promise cancellable while adopting a pending
            // value. These intrinsic reactions only settle it; user code
            // still runs exclusively through the continuation scheduler.
            const adopted = nativeResolve(value);
            deriving = true;
            try { apply(nativeThen, adopted, [fulfilled, rejected]); } finally { deriving = false; }
          } else fulfilled(value);
        };
        const fail = error => { if (!locked) { locked = true; rejected(error); } };
        try { executor(accept, fail); } catch (error) { fail(error); }
      }], newTarget);
      creators.set(promise, currentCall());
      return own(promise);
    },
  });
  // Registering a reaction captures its consumer's identity. The native
  // callback queues a Rust continuation instead of running plugin JS in
  // JSC's shared microtask drain; a terminated turn cannot drop siblings.
  // Cancelling the consumer interrupts the wait with an AbortError; the
  // reaction is one-shot, so a source settling later is ignored.
  NativePromise.prototype.then = function (done, failed) {
    const source = this;
    return derived((resolve, reject) => {
      let settled = false;
      let token = 0;
      const fulfilled = value => {
        if (settled) return;
        settled = true;
        unregisterCancel(token);
        try { resolve(typeof done === "function" ? done(value) : value); } catch (error) { reject(error); }
      };
      const rejected = error => {
        if (settled) return;
        settled = true;
        unregisterCancel(token);
        try { if (typeof failed === "function") resolve(failed(error)); else reject(error); } catch (error) { reject(error); }
      };
      if (creators.get(source) !== currentCall()) token = registerCancel(rejected, true);
      deriving = true;
      try { apply(nativeThen, source, [hostBind(fulfilled, false), hostBind(rejected, false)]); } finally { deriving = false; }
    });
  };
  Object.defineProperty(NativePromise.prototype, "constructor", { value: Promise });
  globalThis.Promise = Promise;
  globalThis.queueMicrotask = fn => {
    if (typeof fn !== "function") throw new TypeError("callback must be a function");
    Promise.resolve().then(hostBind(fn, true));
  };
function OverloadYield(e, d) {
  this.v = e, this.k = d;
}

function asyncGeneratorStep(n, t, e, r, o, a, c, f) {
  running.push(f);
  try {
    var i = n[a](c),
      u = i.value;
  } catch (n) {
    return void e(n);
  } finally {
    running.pop();
  }
  i.done ? t(u) : Promise.resolve(u).then(r, o);
}
function asyncToGenerator(n) {
  return function () {
    var t = this,
      e = arguments;
    return derived(function (r, o) {
      var a = n.apply(t, e);
      // A frame that existed when its call was cancelled sees an AbortError
      // once: on its first resumption after that, or as a host call throws
      // it. A frame started during the call's cleanup never does, so cleanup
      // code awaits normally.
      var f = { reject: o, observed: callCancelled() };
      function _next(n) {
        if (!f.observed && callCancelled()) {
          f.observed = true;
          return void asyncGeneratorStep(a, r, o, _next, _throw, "throw", cancelledError(), f);
        }
        asyncGeneratorStep(a, r, o, _next, _throw, "next", n, f);
      }
      function _throw(n) {
        f.observed = f.observed || callCancelled();
        asyncGeneratorStep(a, r, o, _next, _throw, "throw", n, f);
      }
      _next(void 0);
    });
  };
}

function wrapAsyncGenerator(e) {
  function wrapped() {
    const iterator = new AsyncGenerator(e.apply(this, arguments));
    Object.setPrototypeOf(iterator, wrapped.prototype);
    return iterator;
  }
  Object.setPrototypeOf(wrapped, AsyncGeneratorFunctionPrototype);
  wrapped.prototype = Object.create(AsyncGenerator.prototype);
  return wrapped;
}
function AsyncGenerator(e) {
  var r, t;
  function resume(r, t) {
    try {
      var n = e[r](t),
        o = n.value,
        u = o instanceof OverloadYield;
      Promise.resolve(u ? o.v : o).then(function (t) {
        if (u) {
          var i = "return" === r ? "return" : "next";
          if (!o.k || t.done) return resume(i, t);
          t = e[i](t).value;
        }
        settle(n.done ? "return" : "normal", t);
      }, function (e) {
        resume("throw", e);
      });
    } catch (e) {
      settle("throw", e);
    }
  }
  function settle(e, n) {
    switch (e) {
      case "return":
        r.resolve({
          value: n,
          done: !0
        });
        break;
      case "throw":
        r.reject(n);
        break;
      default:
        r.resolve({
          value: n,
          done: !1
        });
    }
    (r = r.next) ? resume(r.key, r.arg) : t = null;
  }
  this._invoke = function (e, n) {
    return derived(function (o, u) {
      var i = {
        key: e,
        arg: n,
        resolve: o,
        reject: u,
        next: null
      };
      t ? t = t.next = i : (r = t = i, resume(e, n));
    });
  }, "function" != typeof e["return"] && (this["return"] = void 0);
}
AsyncGenerator.prototype["function" == typeof Symbol && Symbol.asyncIterator || "@@asyncIterator"] = function () {
  return this;
}, AsyncGenerator.prototype.next = function (e) {
  return this._invoke("next", e);
}, AsyncGenerator.prototype["throw"] = function (e) {
  return this._invoke("throw", e);
}, AsyncGenerator.prototype["return"] = function (e) {
  return this._invoke("return", e);
};

function awaitAsyncGenerator(e) {
  return new OverloadYield(e, 0);
}

function asyncGeneratorDelegate(t) {
  var e = {},
    n = !1;
  function pump(e, r) {
    return n = !0, r = derived(function (n) {
      n(t[e](r));
    }), {
      done: !1,
      value: new OverloadYield(r, 1)
    };
  }
  return e["undefined" != typeof Symbol && Symbol.iterator || "@@iterator"] = function () {
    return this;
  }, e.next = function (t) {
    return n ? (n = !1, t) : pump("next", t);
  }, "function" == typeof t["throw"] && (e["throw"] = function (t) {
    if (n) throw n = !1, t;
    return pump("throw", t);
  }), "function" == typeof t["return"] && (e["return"] = function (t) {
    return n ? (n = !1, t) : pump("return", t);
  }), e;
}

function asyncIterator(r) {
  var n,
    t,
    o,
    e = 2;
  for ("undefined" != typeof Symbol && (t = Symbol.asyncIterator, o = Symbol.iterator); e--;) {
    if (t && null != (n = r[t])) return n.call(r);
    if (o && null != (n = r[o])) return new AsyncFromSyncIterator(n.call(r));
    t = "@@asyncIterator", o = "@@iterator";
  }
  throw new TypeError("Object is not async iterable");
}
function AsyncFromSyncIterator(r) {
  function AsyncFromSyncIteratorContinuation(r) {
    if (Object(r) !== r) return Promise.reject(new TypeError(r + " is not an object."));
    var n = r.done;
    return Promise.resolve(r.value).then(function (r) {
      return {
        value: r,
        done: n
      };
    });
  }
  return AsyncFromSyncIterator = function AsyncFromSyncIterator(r) {
    this.s = r, this.n = r.next;
  }, AsyncFromSyncIterator.prototype = {
    s: null,
    n: null,
    next: function next() {
      return AsyncFromSyncIteratorContinuation(this.n.apply(this.s, arguments));
    },
    "return": function _return(r) {
      var n = this.s["return"];
      return void 0 === n ? Promise.resolve({
        value: r,
        done: !0
      }) : AsyncFromSyncIteratorContinuation(n.apply(this.s, arguments));
    },
    "throw": function _throw(r) {
      var n = this.s["return"];
      return void 0 === n ? Promise.reject(r) : AsyncFromSyncIteratorContinuation(n.apply(this.s, arguments));
    }
  }, new AsyncFromSyncIterator(r);
}

  // Preserve the prototype shape used by Web Streams to discover the
  // shared AsyncIteratorPrototype, even after generator lowering.
  const AsyncIteratorPrototype = { [Symbol.asyncIterator]() { return this; } };
  Object.setPrototypeOf(AsyncGenerator.prototype, AsyncIteratorPrototype);
  Object.defineProperty(AsyncGenerator.prototype, Symbol.toStringTag, { value: "AsyncGenerator" });
  const AsyncGeneratorFunctionPrototype = Object.create(Function.prototype);
  Object.defineProperty(AsyncGeneratorFunctionPrototype, "prototype", { value: AsyncGenerator.prototype });
  Object.defineProperty(AsyncGeneratorFunctionPrototype, Symbol.toStringTag, { value: "AsyncGeneratorFunction" });
  return Object.freeze({
    asyncToGenerator, wrapAsyncGenerator, awaitAsyncGenerator, asyncGeneratorDelegate, asyncIterator,
    bind: hostBind,
    // After a terminated turn: reject the async frames it left mid-execution.
    abortRunning(error) { for (const frame of running.splice(0).reverse()) frame.reject(error); },
    // A host call threw this turn's AbortError through the running frames.
    observeCancel() { for (const frame of running) frame.observed = true; },
    setUnhandled(fn) { unhandled = fn; },
    onUnhandled(promise, reason) { const report = owners.get(promise); if (report) report(reason); },
  });
});
