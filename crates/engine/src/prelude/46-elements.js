// Part 4c: the remaining element interfaces. Links, scripts, stylesheets,
// images and media, each with what script reads from it. Interfaces with no
// members of their own are created on demand by `elementInterface`.

// Members derived from a URL attribute, shared by <a> and <area>.
function urlMembers(attribute) {
  const parts = (el) => {
    try {
      return new URL(el.getAttribute(attribute) ?? '', api.url());
    } catch (e) {
      return null;
    }
  };
  const members = {
    toString() {
      return this[attribute];
    },
  };
  for (const part of ['origin', 'protocol', 'username', 'password', 'host', 'hostname', 'port', 'pathname', 'search', 'hash']) {
    Object.defineProperty(members, part, {
      get() {
        const u = parts(this);
        return u ? u[part] : '';
      },
      set(value) {
        const u = parts(this);
        if (!u || part === 'origin') return;
        u[part] = value;
        this.setAttribute(attribute, u.href);
      },
      enumerable: true,
      configurable: true,
    });
  }
  return members;
}

const HTMLAnchorElement = defineElement('HTMLAnchorElement', HTMLElement, urlMembers('href'), {
  get text() {
    return api.textContent(own(this));
  },
  set text(v) {
    replaceText(this, v);
  },
  get relList() {
    return tokenList(this, 'rel');
  },
});
reflect(HTMLAnchorElement, 'url', { href: 'href' });
reflect(HTMLAnchorElement, 'string', { target: 'target', rel: 'rel', download: 'download', hreflang: 'hreflang', type: 'type', referrerPolicy: 'referrerpolicy', ping: 'ping' });

const HTMLAreaElement = defineElement('HTMLAreaElement', HTMLElement, urlMembers('href'));
reflect(HTMLAreaElement, 'url', { href: 'href' });
reflect(HTMLAreaElement, 'string', { alt: 'alt', target: 'target', rel: 'rel', download: 'download', shape: 'shape', coords: 'coords' });

const HTMLScriptElement = defineElement('HTMLScriptElement', HTMLElement, {
  get text() {
    return api.textContent(own(this));
  },
  set text(v) {
    replaceText(this, v);
  },
  // A script made by script is async unless told otherwise, as in a browser.
  get async() {
    return this.hasAttribute('async') || (!parserScripts.has(own(this)) && !forcedSync.has(this));
  },
  set async(v) {
    if (v) this.setAttribute('async', '');
    else {
      this.removeAttribute('async');
      forcedSync.add(this);
    }
  },
});
const forcedSync = new WeakSet();
HTMLScriptElement.supports = (type) => type === 'classic';
reflect(HTMLScriptElement, 'url', { src: 'src' });
reflect(HTMLScriptElement, 'string', { type: 'type', charset: 'charset', crossOrigin: 'crossorigin', integrity: 'integrity', referrerPolicy: 'referrerpolicy', event: 'event', htmlFor: 'for' });
reflect(HTMLScriptElement, 'boolean', { defer: 'defer', noModule: 'nomodule' });

// ---- stylesheets ------------------------------------------------------------
//
// There is no cascade, so rules are kept but not applied. They exist because
// CSS-in-JS libraries insert rules through the CSSOM in production builds and
// fail loudly when there is no sheet to insert into.

class CSSStyleSheet {
  constructor() {
    this.cssRules = [];
    this.ownerNode = null;
    this.disabled = false;
    this.href = null;
    this.media = [];
    this.type = 'text/css';
  }
  get rules() {
    return this.cssRules;
  }
  insertRule(rule, index = 0) {
    if (index < 0 || index > this.cssRules.length) throw domError('IndexSizeError', `The index provided (${index}) is outside the range [0, ${this.cssRules.length}].`);
    this.cssRules.splice(index, 0, { cssText: String(rule), type: 1 });
    return index;
  }
  deleteRule(index) {
    this.cssRules.splice(index, 1);
  }
  addRule(selector, style, index = this.cssRules.length) {
    return this.insertRule(`${selector} { ${style} }`, index);
  }
  removeRule(index) {
    this.deleteRule(index);
  }
  replaceSync(text) {
    this.cssRules = [{ cssText: String(text), type: 1 }];
  }
  replace(text) {
    this.replaceSync(text);
    return Promise.resolve(this);
  }
}
tagClass(CSSStyleSheet, 'CSSStyleSheet');

const sheets = new WeakMap();
function sheetFor(el) {
  let sheet = sheets.get(el);
  if (!sheet) {
    sheet = new CSSStyleSheet();
    sheet.ownerNode = el;
    sheets.set(el, sheet);
  }
  if (el.localName === 'link') sheet.href = el.href;
  return sheet;
}

const isStylesheetLink = (id) => /(^|\s)stylesheet(\s|$)/i.test(api.attr(id, 'rel') ?? '') && api.hasAttr(id, 'href');

function documentStyleSheets() {
  return wrapAll(
    api.byTag(documentId, '*').filter((id) => {
      const tag = api.tagName(id);
      return tag === 'style' || (tag === 'link' && isStylesheetLink(id));
    }),
  ).map(sheetFor);
}

// A stylesheet <link> added by script is fetched, and fires `load` or
// `error`. Bundlers load CSS chunks this way and wait for the event, so a
// link that never settles would hang the page. The rules are not applied.
const requestedLinks = new Set();
function linkInserted(id) {
  if (requestedLinks.has(id) || !isStylesheetLink(id)) return;
  requestedLinks.add(id);
  const el = wrap(id);
  const href = el.href;
  schedule(() => {
    const response = performRequest('GET', href, ['Accept', 'text/css,*/*;q=0.1'], '');
    fire(el, response.status >= 200 && response.status < 300 ? 'load' : 'error');
  }, 0);
}

const HTMLLinkElement = defineElement('HTMLLinkElement', HTMLElement, {
  get sheet() {
    return isStylesheetLink(own(this)) && this.isConnected ? sheetFor(this) : null;
  },
  get relList() {
    return tokenList(this, 'rel');
  },
});
reflect(HTMLLinkElement, 'url', { href: 'href' });
reflect(HTMLLinkElement, 'string', { rel: 'rel', as: 'as', type: 'type', media: 'media', crossOrigin: 'crossorigin', integrity: 'integrity', hreflang: 'hreflang', sizes: 'sizes', referrerPolicy: 'referrerpolicy' });
reflect(HTMLLinkElement, 'boolean', { disabled: 'disabled' });

const HTMLStyleElement = defineElement('HTMLStyleElement', HTMLElement, {
  get sheet() {
    return this.isConnected ? sheetFor(this) : null;
  },
});
reflect(HTMLStyleElement, 'string', { media: 'media', type: 'type' });
reflect(HTMLStyleElement, 'boolean', { disabled: 'disabled' });

// ---- images and media -------------------------------------------------------

// Images are not fetched, so they never load and have no natural size.
const HTMLImageElement = defineElement('HTMLImageElement', HTMLElement, {
  get width() {
    return parseInt(this.getAttribute('width'), 10) || 0;
  },
  set width(v) {
    this.setAttribute('width', String(v));
  },
  get height() {
    return parseInt(this.getAttribute('height'), 10) || 0;
  },
  set height(v) {
    this.setAttribute('height', String(v));
  },
  get naturalWidth() {
    return 0;
  },
  get naturalHeight() {
    return 0;
  },
  get complete() {
    return true;
  },
  get currentSrc() {
    return this.src;
  },
  decode() {
    return Promise.resolve();
  },
});
reflect(HTMLImageElement, 'url', { src: 'src' });
reflect(HTMLImageElement, 'string', { alt: 'alt', srcset: 'srcset', sizes: 'sizes', crossOrigin: 'crossorigin', useMap: 'usemap', loading: 'loading', decoding: 'decoding', referrerPolicy: 'referrerpolicy' });
reflect(HTMLImageElement, 'boolean', { isMap: 'ismap' });

// There is no rasterizer. A '2d' context still exists, because a browser
// always provides one and libraries draw into it at load without checking;
// it accepts every call and draws nothing. Pixels read back are transparent,
// and text is measured from the font size. WebGL returns null, which is what
// a browser returns when WebGL is unavailable.
class CanvasRenderingContext2D {
  constructor() {
    illegalConstructor();
  }
}
tagClass(CanvasRenderingContext2D, 'CanvasRenderingContext2D');
const DRAWING_CALLS = (
  'arc arcTo beginPath bezierCurveTo clearRect clip closePath drawFocusIfNeeded drawImage ellipse fill ' +
  'fillRect fillText lineTo moveTo putImageData quadraticCurveTo rect reset resetTransform restore rotate ' +
  'roundRect save scale setLineDash setTransform stroke strokeRect strokeText transform translate'
).split(' ');
const contexts = new WeakMap();
function context2d(canvas) {
  let ctx = contexts.get(canvas);
  if (ctx) return ctx;
  ctx = Object.create(CanvasRenderingContext2D.prototype);
  for (const name of DRAWING_CALLS) ctx[name] = () => {};
  const fontSize = () => parseFloat(/(\d+(?:\.\d+)?)px/.exec(ctx.font)?.[1] ?? '10');
  Object.assign(ctx, {
    canvas,
    fillStyle: '#000000',
    strokeStyle: '#000000',
    lineWidth: 1,
    lineCap: 'butt',
    lineJoin: 'miter',
    miterLimit: 10,
    lineDashOffset: 0,
    font: '10px sans-serif',
    textAlign: 'start',
    textBaseline: 'alphabetic',
    direction: 'ltr',
    globalAlpha: 1,
    globalCompositeOperation: 'source-over',
    imageSmoothingEnabled: true,
    shadowBlur: 0,
    shadowColor: 'rgba(0, 0, 0, 0)',
    shadowOffsetX: 0,
    shadowOffsetY: 0,
    filter: 'none',
    getLineDash: () => [],
    isPointInPath: () => false,
    isPointInStroke: () => false,
    getTransform: () => ({ a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 }),
    getContextAttributes: () => ({ alpha: true, desynchronized: false }),
    createLinearGradient: () => ({ addColorStop() {} }),
    createRadialGradient: () => ({ addColorStop() {} }),
    createConicGradient: () => ({ addColorStop() {} }),
    createPattern: () => ({ setTransform() {} }),
    createImageData: (w, h) => imageData(typeof w === 'object' ? w.width : w, typeof w === 'object' ? w.height : h),
    getImageData: (x, y, w, h) => imageData(w, h),
    measureText(text) {
      const size = fontSize();
      const width = String(text).length * size * 0.55;
      return {
        width,
        actualBoundingBoxLeft: 0,
        actualBoundingBoxRight: width,
        actualBoundingBoxAscent: size * 0.8,
        actualBoundingBoxDescent: size * 0.2,
        fontBoundingBoxAscent: size * 0.8,
        fontBoundingBoxDescent: size * 0.2,
      };
    },
  });
  contexts.set(canvas, ctx);
  return ctx;
}
function imageData(width, height) {
  width = Math.max(1, width | 0);
  height = Math.max(1, height | 0);
  return { width, height, colorSpace: 'srgb', data: new Uint8ClampedArray(width * height * 4) };
}

const HTMLCanvasElement = defineElement('HTMLCanvasElement', HTMLElement, {
  get width() {
    return parseInt(this.getAttribute('width'), 10) || 300;
  },
  set width(v) {
    this.setAttribute('width', String(v));
  },
  get height() {
    return parseInt(this.getAttribute('height'), 10) || 150;
  },
  set height(v) {
    this.setAttribute('height', String(v));
  },
  getContext(type) {
    return String(type) === '2d' ? context2d(this) : null;
  },
  toDataURL() {
    return 'data:,';
  },
  toBlob(callback) {
    schedule(() => callback(null), 0);
  },
});

const HTMLMediaElement = defineElement('HTMLMediaElement', HTMLElement, {
  get paused() {
    return true;
  },
  get currentTime() {
    return 0;
  },
  set currentTime(v) {},
  get duration() {
    return NaN;
  },
  get readyState() {
    return 0;
  },
  get volume() {
    return 1;
  },
  set volume(v) {},
  get muted() {
    return this.hasAttribute('muted');
  },
  set muted(v) {
    this.toggleAttribute('muted', !!v);
  },
  play() {
    return Promise.reject(domError('NotSupportedError', 'Media playback is not available in a headless engine.'));
  },
  pause() {},
  load() {},
  canPlayType() {
    return '';
  },
});
reflect(HTMLMediaElement, 'url', { src: 'src' });
reflect(HTMLMediaElement, 'boolean', { autoplay: 'autoplay', controls: 'controls', loop: 'loop' });
defineElement('HTMLVideoElement', HTMLMediaElement);
defineElement('HTMLAudioElement', HTMLMediaElement);

// ---- the rest ---------------------------------------------------------------

const HTMLDialogElement = defineElement('HTMLDialogElement', HTMLElement, {
  show() {
    this.setAttribute('open', '');
  },
  showModal() {
    this.setAttribute('open', '');
  },
  close(returnValue) {
    if (!this.hasAttribute('open')) return;
    if (returnValue !== undefined) this.returnValue = String(returnValue);
    this.removeAttribute('open');
    fire(this, 'close');
  },
});
HTMLDialogElement.prototype.returnValue = '';
reflect(HTMLDialogElement, 'boolean', { open: 'open' });

const HTMLDetailsElement = defineElement('HTMLDetailsElement', HTMLElement);
reflect(HTMLDetailsElement, 'boolean', { open: 'open' });

// Frames are not loaded; a frame has no window or document to reach into.
const HTMLIFrameElement = defineElement('HTMLIFrameElement', HTMLElement, {
  get contentWindow() {
    return null;
  },
  get contentDocument() {
    return null;
  },
});
reflect(HTMLIFrameElement, 'url', { src: 'src' });
reflect(HTMLIFrameElement, 'string', { name: 'name', srcdoc: 'srcdoc', allow: 'allow', width: 'width', height: 'height', loading: 'loading' });

const HTMLTemplateElement = defineElement('HTMLTemplateElement', HTMLElement, {
  // The parser keeps a template's contents apart; script gets a copy of them
  // as a fragment, which is what cloning `content` needs.
  get content() {
    return parseMarkup(own(this), api.innerHTML(own(this)), false);
  },
});

const HTMLMetaElement = defineElement('HTMLMetaElement', HTMLElement);
reflect(HTMLMetaElement, 'string', { name: 'name', content: 'content', httpEquiv: 'http-equiv', charset: 'charset', media: 'media' });

defineElement('HTMLTitleElement', HTMLElement, {
  get text() {
    return api.textContent(own(this));
  },
  set text(v) {
    replaceText(this, v);
  },
});

const HTMLTableCellElement = defineElement('HTMLTableCellElement', HTMLElement);
reflect(HTMLTableCellElement, 'number', { colSpan: 'colspan', rowSpan: 'rowspan' });
reflect(HTMLTableCellElement, 'string', { headers: 'headers', scope: 'scope', abbr: 'abbr' });
defineElement('HTMLTableRowElement', HTMLElement, {
  get cells() {
    return htmlCollection(this.children.filter((c) => c.localName === 'td' || c.localName === 'th'));
  },
});
defineElement('HTMLTableElement', HTMLElement, {
  get rows() {
    return htmlCollection(wrapAll(api.byTag(own(this), 'tr')));
  },
});
defineElement('SVGSVGElement', SVGElement);
