//! Selectors: parsing, matching and specificity.
//!
//! Selectors Level 4 as far as stylesheets and clients use it: type, `*`,
//! `#id`, `.class`, attribute selectors with every operator and the `i`
//! flag, the four combinators, the structural pseudo-classes including
//! `:nth-child(An+B of S)`, `:not`, `:is`, `:where`, `:has`, and the form
//! state pseudo-classes. The same parsed selectors serve `querySelector` and
//! the cascade.
//!
//! What cannot be true without a pointer or a history (`:hover`, `:active`,
//! `:visited`) never matches. A selector with a pseudo-element (`::before`)
//! names something that is not an element, so it never matches an element;
//! the cascade skips such rules. Anything unparseable makes its whole
//! selector unmatchable, which is safer than matching the wrong element.

use crate::dom::{Document, NodeData, NodeId};

/// A comma-separated list. It matches when any of its selectors does.
pub type SelectorList = Vec<Selector>;

/// One complex selector: compounds joined by combinators, rightmost last.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    /// `parts[0]` is the leftmost compound. `parts[i].0` is the combinator
    /// between `parts[i-1]` and `parts[i]`; the first one's is unused.
    pub parts: Vec<(Combinator, Compound)>,
    /// Set when the selector ends in a pseudo-element, such as `a::before`.
    pub pseudo_element: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    Descendant,
    Child,
    NextSibling,
    SubsequentSibling,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Compound {
    /// Lowercase local name; `None` for `*` or no type at all.
    pub tag: Option<String>,
    pub simple: Vec<Simple>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Simple {
    Id(String),
    Class(String),
    Attr { name: String, op: AttrOp, value: String, case_insensitive: bool },
    Pseudo(Pseudo),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrOp {
    Exists,
    Equals,
    Includes,
    DashMatch,
    Prefix,
    Suffix,
    Substring,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pseudo {
    Not(SelectorList),
    Is(SelectorList),
    Where(SelectorList),
    /// `:has()`. Each selector is relative; its first combinator says where
    /// to look from the subject (descendants by default).
    Has(SelectorList),
    Nth { a: i32, b: i32, of_type: bool, from_end: bool, of: Option<SelectorList> },
    Root,
    Empty,
    Scope,
    Checked,
    Disabled,
    Enabled,
    Required,
    Optional,
    ReadOnly,
    ReadWrite,
    PlaceholderShown,
    AnyLink,
    Focus,
    FocusWithin,
    Defined,
    Lang(String),
    /// Something that is never true here: `:hover`, `:visited`, vendor
    /// pseudo-classes.
    Never,
}

/// Parses a selector list. `None` when any part of it is invalid, as a
/// browser rejects the whole list (`querySelector` then throws).
pub fn parse_list(text: &str) -> Option<SelectorList> {
    let mut parser = Parser { chars: text.chars().collect(), pos: 0 };
    let list = parser.list(false)?;
    parser.skip_ws();
    (parser.pos == parser.chars.len()).then_some(list)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn skip_ws(&mut self) -> bool {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
        // Comments count as whitespace inside stylesheets.
        if self.peek() == Some('/') && self.peek_at(1) == Some('*') {
            self.pos += 2;
            while self.pos < self.chars.len() && !(self.peek() == Some('*') && self.peek_at(1) == Some('/')) {
                self.pos += 1;
            }
            self.pos = (self.pos + 2).min(self.chars.len());
            self.skip_ws();
        }
        self.pos > start
    }

    /// A comma-separated list, up to the end or, inside a pseudo-class, a `)`.
    /// `relative` allows a leading combinator, as `:has(> img)` has.
    fn list(&mut self, relative: bool) -> Option<SelectorList> {
        let mut list = Vec::new();
        loop {
            self.skip_ws();
            list.push(self.complex(relative)?);
            self.skip_ws();
            if self.peek() == Some(',') {
                self.pos += 1;
                continue;
            }
            return Some(list);
        }
    }

    fn complex(&mut self, relative: bool) -> Option<Selector> {
        let mut parts = Vec::new();
        let mut pseudo_element = false;
        let mut combinator = Combinator::Descendant;
        if relative {
            if let Some(c) = self.combinator_char() {
                combinator = c;
                self.pos += 1;
                self.skip_ws();
            }
        }
        loop {
            if pseudo_element {
                // Nothing may follow a pseudo-element but its own state.
                return None;
            }
            let (compound, ends_in_pseudo_element) = self.compound()?;
            pseudo_element = ends_in_pseudo_element;
            parts.push((combinator, compound));
            let had_ws = self.skip_ws();
            match self.peek() {
                None | Some(',') | Some(')') => break,
                Some(_) => {
                    if let Some(c) = self.combinator_char() {
                        combinator = c;
                        self.pos += 1;
                        self.skip_ws();
                    } else if had_ws {
                        combinator = Combinator::Descendant;
                    } else {
                        return None;
                    }
                }
            }
        }
        Some(Selector { parts, pseudo_element })
    }

    fn combinator_char(&self) -> Option<Combinator> {
        match self.peek()? {
            '>' => Some(Combinator::Child),
            '+' => Some(Combinator::NextSibling),
            '~' => Some(Combinator::SubsequentSibling),
            _ => None,
        }
    }

    /// One compound. Also reports whether it ended in a pseudo-element.
    fn compound(&mut self) -> Option<(Compound, bool)> {
        let mut compound = Compound::default();
        let start = self.pos;
        if self.peek() == Some('*') {
            self.pos += 1;
        } else if self.peek().is_some_and(is_ident_start) {
            compound.tag = Some(self.ident()?.to_ascii_lowercase());
        }
        // A namespace prefix (`svg|rect`, `*|a`) is accepted and ignored.
        if self.peek() == Some('|') && self.peek_at(1) != Some('=') {
            self.pos += 1;
            if self.peek() == Some('*') {
                self.pos += 1;
                compound.tag = None;
            } else {
                compound.tag = Some(self.ident()?.to_ascii_lowercase());
            }
        }
        loop {
            match self.peek() {
                Some('#') => {
                    self.pos += 1;
                    compound.simple.push(Simple::Id(self.ident()?));
                }
                Some('.') => {
                    self.pos += 1;
                    compound.simple.push(Simple::Class(self.ident()?));
                }
                Some('[') => {
                    self.pos += 1;
                    compound.simple.push(self.attribute()?);
                }
                Some(':') if self.peek_at(1) == Some(':') => {
                    self.pos += 2;
                    self.pseudo_element_name()?;
                    return Some((compound, true));
                }
                Some(':') => {
                    self.pos += 1;
                    let name = self.ident()?.to_ascii_lowercase();
                    // The legacy single-colon pseudo-elements.
                    if matches!(name.as_str(), "before" | "after" | "first-line" | "first-letter") {
                        return Some((compound, true));
                    }
                    compound.simple.push(Simple::Pseudo(self.pseudo_class(&name)?));
                }
                _ => break,
            }
        }
        (self.pos > start).then_some((compound, false))
    }

    fn pseudo_element_name(&mut self) -> Option<()> {
        self.ident()?;
        // `::part(x)`, `::slotted(x)` and friends: skip the argument.
        if self.peek() == Some('(') {
            self.skip_parenthesised()?;
        }
        Some(())
    }

    fn skip_parenthesised(&mut self) -> Option<()> {
        let mut depth = 0;
        while let Some(c) = self.peek() {
            self.pos += 1;
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(());
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn attribute(&mut self) -> Option<Simple> {
        self.skip_ws();
        let mut name = self.ident()?;
        if self.peek() == Some('|') && self.peek_at(1) != Some('=') {
            self.pos += 1;
            name = self.ident()?;
        }
        let name = name.to_ascii_lowercase();
        self.skip_ws();
        let op = match (self.peek()?, self.peek_at(1)) {
            (']', _) => {
                self.pos += 1;
                return Some(Simple::Attr { name, op: AttrOp::Exists, value: String::new(), case_insensitive: false });
            }
            ('=', _) => AttrOp::Equals,
            ('~', Some('=')) => AttrOp::Includes,
            ('|', Some('=')) => AttrOp::DashMatch,
            ('^', Some('=')) => AttrOp::Prefix,
            ('$', Some('=')) => AttrOp::Suffix,
            ('*', Some('=')) => AttrOp::Substring,
            _ => return None,
        };
        self.pos += if op == AttrOp::Equals { 1 } else { 2 };
        self.skip_ws();
        let value = match self.peek()? {
            '"' | '\'' => self.string()?,
            _ => self.ident()?,
        };
        self.skip_ws();
        let mut case_insensitive = false;
        if matches!(self.peek(), Some('i') | Some('I') | Some('s') | Some('S')) {
            case_insensitive = matches!(self.peek(), Some('i') | Some('I'));
            self.pos += 1;
            self.skip_ws();
        }
        (self.peek() == Some(']')).then(|| self.pos += 1)?;
        Some(Simple::Attr { name, op, value, case_insensitive })
    }

    fn pseudo_class(&mut self, name: &str) -> Option<Pseudo> {
        let functional = self.peek() == Some('(');
        if functional {
            self.pos += 1;
            self.skip_ws();
        }
        let pseudo = match (name, functional) {
            ("not", true) => Pseudo::Not(self.list(false)?),
            ("is" | "matches" | "-webkit-any" | "-moz-any", true) => Pseudo::Is(self.list(false)?),
            ("where", true) => Pseudo::Where(self.list(false)?),
            ("has", true) => Pseudo::Has(self.list(true)?),
            ("nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type", true) => {
                let (a, b) = self.an_plus_b()?;
                self.skip_ws();
                let mut of = None;
                if name.ends_with("child") && self.peek() == Some('o') && self.peek_at(1) == Some('f') {
                    self.pos += 2;
                    of = Some(self.list(false)?);
                }
                Pseudo::Nth { a, b, of_type: name.ends_with("of-type"), from_end: name.contains("last"), of }
            }
            ("lang", true) => Pseudo::Lang(self.ident()?.to_ascii_lowercase()),
            (_, true) => {
                // An unknown functional pseudo-class never matches.
                self.pos -= 1;
                self.skip_parenthesised()?;
                return Some(Pseudo::Never);
            }
            ("first-child", _) => Pseudo::Nth { a: 0, b: 1, of_type: false, from_end: false, of: None },
            ("last-child", _) => Pseudo::Nth { a: 0, b: 1, of_type: false, from_end: true, of: None },
            ("first-of-type", _) => Pseudo::Nth { a: 0, b: 1, of_type: true, from_end: false, of: None },
            ("last-of-type", _) => Pseudo::Nth { a: 0, b: 1, of_type: true, from_end: true, of: None },
            ("only-child", _) => Pseudo::Is(vec![only("first-child", "last-child")]),
            ("only-of-type", _) => Pseudo::Is(vec![only("first-of-type", "last-of-type")]),
            ("root", _) => Pseudo::Root,
            ("empty", _) => Pseudo::Empty,
            ("scope", _) => Pseudo::Scope,
            ("checked", _) => Pseudo::Checked,
            ("disabled", _) => Pseudo::Disabled,
            ("enabled", _) => Pseudo::Enabled,
            ("required", _) => Pseudo::Required,
            ("optional", _) => Pseudo::Optional,
            ("read-only", _) => Pseudo::ReadOnly,
            ("read-write", _) => Pseudo::ReadWrite,
            ("placeholder-shown", _) => Pseudo::PlaceholderShown,
            ("link" | "any-link", _) => Pseudo::AnyLink,
            ("focus" | "focus-visible", _) => Pseudo::Focus,
            ("focus-within", _) => Pseudo::FocusWithin,
            ("defined", _) => Pseudo::Defined,
            _ => Pseudo::Never,
        };
        if functional {
            self.skip_ws();
            (self.peek() == Some(')')).then(|| self.pos += 1)?;
        }
        Some(pseudo)
    }

    /// `An+B`, `odd`, `even`, or a plain integer.
    fn an_plus_b(&mut self) -> Option<(i32, i32)> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == ')' || (c == 'o' && self.peek_at(1) == Some('f') && self.pos > start) {
                break;
            }
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().filter(|c| !c.is_whitespace()).collect();
        let text = text.to_ascii_lowercase();
        match text.as_str() {
            "odd" => return Some((2, 1)),
            "even" => return Some((2, 0)),
            _ => {}
        }
        let Some(n_at) = text.find('n') else { return Some((0, text.parse().ok()?)) };
        let a = match &text[..n_at] {
            "" | "+" => 1,
            "-" => -1,
            other => other.parse().ok()?,
        };
        let rest = &text[n_at + 1..];
        let b = if rest.is_empty() { 0 } else { rest.parse().ok()? };
        Some((a, b))
    }

    fn ident(&mut self) -> Option<String> {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '\\' {
                self.pos += 1;
                out.push(self.escape()?);
            } else if is_ident_char(c) || (out.is_empty() && c == '-') {
                out.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// The character after a backslash: a hex code point, or the character.
    fn escape(&mut self) -> Option<char> {
        let mut hex = String::new();
        while hex.len() < 6 && self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
            hex.push(self.peek()?);
            self.pos += 1;
        }
        if hex.is_empty() {
            let c = self.peek()?;
            self.pos += 1;
            return Some(c);
        }
        if self.peek().is_some_and(|c| c.is_whitespace()) {
            self.pos += 1;
        }
        Some(char::from_u32(u32::from_str_radix(&hex, 16).ok()?).unwrap_or('\u{fffd}'))
    }

    fn string(&mut self) -> Option<String> {
        let quote = self.peek()?;
        self.pos += 1;
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            self.pos += 1;
            match c {
                c if c == quote => return Some(out),
                '\\' => out.push(self.escape()?),
                c => out.push(c),
            }
        }
    }
}

fn only(first: &str, last: &str) -> Selector {
    let pseudo = |name: &str| {
        let mut p = Parser { chars: format!(":{name}").chars().collect(), pos: 0 };
        p.compound().map(|(c, _)| c.simple).unwrap_or_default()
    };
    let mut simple = pseudo(first);
    simple.extend(pseudo(last));
    Selector { parts: vec![(Combinator::Descendant, Compound { tag: None, simple })], pseudo_element: false }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '-' || c == '\\' || !c.is_ascii()
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || !c.is_ascii()
}

// ---- matching ---------------------------------------------------------------

/// What matching needs beyond the tree: the element `:scope` names, and the
/// focused element for `:focus`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context {
    pub scope: Option<NodeId>,
    pub focus: Option<NodeId>,
}

/// The first match in document order among `root`'s descendants.
pub fn query(doc: &Document, root: NodeId, selector: &str) -> Option<NodeId> {
    query_all(doc, root, selector).into_iter().next()
}

/// Every match among `root`'s descendants, in document order. An invalid
/// selector matches nothing.
pub fn query_all(doc: &Document, root: NodeId, selector: &str) -> Vec<NodeId> {
    let Some(list) = parse_list(selector) else { return Vec::new() };
    let ctx = Context { scope: Some(root), focus: None };
    doc.descendants(root)
        .skip(1)
        .filter(|id| doc.is_element(*id) && matches_list(doc, *id, &list, ctx))
        .collect()
}

/// True when the element matches the selector list.
pub fn matches(doc: &Document, id: NodeId, selector: &str) -> bool {
    let Some(list) = parse_list(selector) else { return false };
    doc.is_element(id) && matches_list(doc, id, &list, Context { scope: Some(id), focus: None })
}

/// [`query_all`] with the focused element known, for `:focus`. `None` when
/// the selector is invalid, which script reports as a SyntaxError.
pub fn query_all_with(doc: &Document, root: NodeId, selector: &str, focus: Option<NodeId>) -> Option<Vec<NodeId>> {
    let list = parse_list(selector)?;
    let ctx = Context { scope: Some(root), focus };
    Some(
        doc.descendants(root)
            .skip(1)
            .filter(|id| doc.is_element(*id) && matches_list(doc, *id, &list, ctx))
            .collect(),
    )
}

/// [`matches`] with the focused element known. `None` when invalid.
pub fn matches_with(doc: &Document, id: NodeId, selector: &str, focus: Option<NodeId>) -> Option<bool> {
    let list = parse_list(selector)?;
    Some(doc.is_element(id) && matches_list(doc, id, &list, Context { scope: Some(id), focus }))
}

pub fn matches_list(doc: &Document, id: NodeId, list: &[Selector], ctx: Context) -> bool {
    list.iter().any(|selector| matches_selector(doc, id, selector, ctx))
}

/// True when the element is the subject of `selector`. A selector ending in
/// a pseudo-element never is.
pub fn matches_selector(doc: &Document, id: NodeId, selector: &Selector, ctx: Context) -> bool {
    !selector.pseudo_element && !selector.parts.is_empty() && match_from(doc, id, &selector.parts, selector.parts.len() - 1, ctx)
}

fn match_from(doc: &Document, id: NodeId, parts: &[(Combinator, Compound)], index: usize, ctx: Context) -> bool {
    if !compound_matches(doc, id, &parts[index].1, ctx) {
        return false;
    }
    if index == 0 {
        return true;
    }
    let next = |candidate: NodeId| match_from(doc, candidate, parts, index - 1, ctx);
    match parts[index].0 {
        Combinator::Child => element_parent(doc, id).is_some_and(next),
        Combinator::Descendant => ancestors(doc, id).any(next),
        Combinator::NextSibling => previous_element(doc, id).is_some_and(next),
        Combinator::SubsequentSibling => previous_elements(doc, id).any(next),
    }
}

fn element_parent(doc: &Document, id: NodeId) -> Option<NodeId> {
    doc.parent(id).filter(|p| doc.is_element(*p))
}

fn ancestors(doc: &Document, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    std::iter::successors(element_parent(doc, id), move |p| element_parent(doc, *p))
}

fn element_siblings(doc: &Document, id: NodeId) -> Vec<NodeId> {
    match doc.parent(id) {
        Some(parent) => doc.children(parent).iter().copied().filter(|c| doc.is_element(*c)).collect(),
        None => vec![id],
    }
}

fn previous_element(doc: &Document, id: NodeId) -> Option<NodeId> {
    let siblings = element_siblings(doc, id);
    let at = siblings.iter().position(|s| *s == id)?;
    at.checked_sub(1).map(|i| siblings[i])
}

fn previous_elements(doc: &Document, id: NodeId) -> impl Iterator<Item = NodeId> {
    let siblings = element_siblings(doc, id);
    let at = siblings.iter().position(|s| *s == id).unwrap_or(0);
    siblings.into_iter().take(at).rev()
}

fn compound_matches(doc: &Document, id: NodeId, compound: &Compound, ctx: Context) -> bool {
    if let Some(tag) = &compound.tag {
        if !doc.tag(id).is_some_and(|t| t.eq_ignore_ascii_case(tag)) {
            return false;
        }
    }
    compound.simple.iter().all(|simple| simple_matches(doc, id, simple, ctx))
}

/// HTML attributes whose values compare case-insensitively.
const CASELESS_ATTRS: &[&str] = &["type", "lang", "dir", "method", "rel", "target", "align", "media", "charset"];

fn simple_matches(doc: &Document, id: NodeId, simple: &Simple, ctx: Context) -> bool {
    match simple {
        Simple::Id(want) => doc.attr(id, "id") == Some(want.as_str()),
        Simple::Class(want) => doc.attr(id, "class").is_some_and(|c| c.split_ascii_whitespace().any(|x| x == want)),
        Simple::Attr { name, op, value, case_insensitive } => {
            let Some(actual) = doc.attr(id, name) else { return false };
            let caseless = *case_insensitive || CASELESS_ATTRS.contains(&name.as_str());
            let (actual, value) = if caseless {
                (actual.to_lowercase(), value.to_lowercase())
            } else {
                (actual.to_string(), value.clone())
            };
            match op {
                AttrOp::Exists => true,
                AttrOp::Equals => actual == value,
                AttrOp::Includes => !value.is_empty() && actual.split_ascii_whitespace().any(|w| w == value),
                AttrOp::DashMatch => actual == value || actual.starts_with(&format!("{value}-")),
                AttrOp::Prefix => !value.is_empty() && actual.starts_with(&value),
                AttrOp::Suffix => !value.is_empty() && actual.ends_with(&value),
                AttrOp::Substring => !value.is_empty() && actual.contains(&value),
            }
        }
        Simple::Pseudo(pseudo) => pseudo_matches(doc, id, pseudo, ctx),
    }
}

fn pseudo_matches(doc: &Document, id: NodeId, pseudo: &Pseudo, ctx: Context) -> bool {
    let tag = doc.tag(id).unwrap_or("");
    match pseudo {
        Pseudo::Not(list) => !matches_list(doc, id, list, ctx),
        Pseudo::Is(list) | Pseudo::Where(list) => matches_list(doc, id, list, ctx),
        Pseudo::Has(list) => list.iter().any(|relative| has_match(doc, id, relative, ctx)),
        Pseudo::Nth { a, b, of_type, from_end, of } => {
            let mut siblings = element_siblings(doc, id);
            if *of_type {
                siblings.retain(|s| doc.tag(*s) == doc.tag(id));
            }
            if let Some(list) = of {
                siblings.retain(|s| matches_list(doc, *s, list, ctx));
            }
            if *from_end {
                siblings.reverse();
            }
            match siblings.iter().position(|s| *s == id) {
                Some(index) => nth(*a, *b, index as i32 + 1),
                None => false,
            }
        }
        Pseudo::Root => doc.parent(id).is_some_and(|p| matches!(doc.node(p).data, NodeData::Document)),
        Pseudo::Empty => doc.children(id).iter().all(|c| match doc.node(*c).data {
            NodeData::Comment => true,
            NodeData::Text => doc.node(*c).text.is_empty(),
            _ => false,
        }),
        Pseudo::Scope => Some(id) == ctx.scope || (ctx.scope.is_none() && pseudo_matches(doc, id, &Pseudo::Root, ctx)),
        Pseudo::Checked => match tag {
            "input" => {
                matches!(doc.attr(id, "type").map(str::to_ascii_lowercase).as_deref(), Some("checkbox" | "radio"))
                    && doc.current_checked(id)
            }
            "option" => doc.control(id).selected.unwrap_or_else(|| doc.has_attr(id, "selected")),
            _ => false,
        },
        Pseudo::Disabled => is_disabled(doc, id),
        Pseudo::Enabled => {
            matches!(tag, "button" | "input" | "select" | "textarea" | "optgroup" | "option" | "fieldset") && !is_disabled(doc, id)
        }
        Pseudo::Required => matches!(tag, "input" | "select" | "textarea") && doc.has_attr(id, "required"),
        Pseudo::Optional => matches!(tag, "input" | "select" | "textarea") && !doc.has_attr(id, "required"),
        Pseudo::ReadWrite => is_read_write(doc, id),
        Pseudo::ReadOnly => !is_read_write(doc, id),
        Pseudo::PlaceholderShown => {
            matches!(tag, "input" | "textarea") && doc.has_attr(id, "placeholder") && doc.current_value(id).is_empty()
        }
        Pseudo::AnyLink => matches!(tag, "a" | "area") && doc.has_attr(id, "href"),
        Pseudo::Focus => ctx.focus == Some(id),
        Pseudo::FocusWithin => ctx.focus.is_some_and(|f| doc.is_inclusive_ancestor(id, f)),
        Pseudo::Defined => true,
        Pseudo::Lang(want) => {
            let lang = std::iter::once(id)
                .chain(ancestors(doc, id))
                .find_map(|n| doc.attr(n, "lang"))
                .unwrap_or("")
                .to_ascii_lowercase();
            lang == *want || lang.starts_with(&format!("{want}-"))
        }
        Pseudo::Never => false,
    }
}

/// Whether position `index` (1-based) is `a*n + b` for some n >= 0.
fn nth(a: i32, b: i32, index: i32) -> bool {
    if a == 0 {
        return index == b;
    }
    let diff = index - b;
    diff % a == 0 && diff / a >= 0
}

fn is_disabled(doc: &Document, id: NodeId) -> bool {
    let tag = doc.tag(id).unwrap_or("");
    if !matches!(tag, "button" | "input" | "select" | "textarea" | "optgroup" | "option" | "fieldset") {
        return false;
    }
    if doc.has_attr(id, "disabled") {
        return true;
    }
    if tag == "option" {
        return element_parent(doc, id).is_some_and(|p| doc.is_tag(p, "optgroup") && doc.has_attr(p, "disabled"));
    }
    // Inside a disabled fieldset, unless inside its first legend.
    ancestors(doc, id).any(|fieldset| {
        doc.is_tag(fieldset, "fieldset") && doc.has_attr(fieldset, "disabled") && {
            let legend = doc.children(fieldset).iter().copied().find(|c| doc.is_element(*c));
            !legend.is_some_and(|l| doc.is_tag(l, "legend") && doc.is_inclusive_ancestor(l, id))
        }
    })
}

fn is_read_write(doc: &Document, id: NodeId) -> bool {
    let tag = doc.tag(id).unwrap_or("");
    let text_input = tag == "textarea"
        || (tag == "input"
            && !matches!(
                doc.attr(id, "type").map(str::to_ascii_lowercase).as_deref(),
                Some("checkbox" | "radio" | "file" | "submit" | "image" | "reset" | "button" | "hidden" | "range" | "color")
            ));
    if text_input {
        return !doc.has_attr(id, "readonly") && !is_disabled(doc, id);
    }
    std::iter::once(id)
        .chain(ancestors(doc, id))
        .find_map(|n| doc.attr(n, "contenteditable"))
        .is_some_and(|v| v.is_empty() || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("plaintext-only"))
}

/// `:has(relative)`, anchored at `anchor`.
fn has_match(doc: &Document, anchor: NodeId, relative: &Selector, ctx: Context) -> bool {
    if relative.pseudo_element || relative.parts.is_empty() {
        return false;
    }
    let siblings_first = matches!(relative.parts[0].0, Combinator::NextSibling | Combinator::SubsequentSibling);
    let candidates: Vec<NodeId> = if siblings_first {
        let siblings = element_siblings(doc, anchor);
        let at = siblings.iter().position(|s| *s == anchor).unwrap_or(siblings.len());
        siblings[at + 1..].iter().flat_map(|s| doc.descendants(*s).filter(|d| doc.is_element(*d))).collect()
    } else {
        doc.descendants(anchor).skip(1).filter(|d| doc.is_element(*d)).collect()
    };
    let last = relative.parts.len() - 1;
    candidates.into_iter().any(|c| match_relative(doc, c, &relative.parts, last, anchor, ctx))
}

fn match_relative(
    doc: &Document,
    id: NodeId,
    parts: &[(Combinator, Compound)],
    index: usize,
    anchor: NodeId,
    ctx: Context,
) -> bool {
    if !compound_matches(doc, id, &parts[index].1, ctx) {
        return false;
    }
    let step = |next: NodeId| {
        if index == 0 {
            next == anchor
        } else {
            match_relative(doc, next, parts, index - 1, anchor, ctx)
        }
    };
    match parts[index].0 {
        Combinator::Child => doc.parent(id).is_some_and(step),
        Combinator::Descendant => std::iter::successors(doc.parent(id), |p| doc.parent(*p)).any(step),
        Combinator::NextSibling => previous_element(doc, id).is_some_and(step),
        Combinator::SubsequentSibling => previous_elements(doc, id).any(step),
    }
}

// ---- specificity --------------------------------------------------------------

impl Selector {
    /// `(ids, classes, types)` packed into one comparable number.
    pub fn specificity(&self) -> u32 {
        let (mut a, mut b, mut c) = (0, 0, 0);
        for (_, compound) in &self.parts {
            let (x, y, z) = compound_specificity(compound);
            a += x;
            b += y;
            c += z;
        }
        if self.pseudo_element {
            c += 1;
        }
        pack(a, b, c)
    }
}

fn pack(a: u32, b: u32, c: u32) -> u32 {
    (a.min(1023) << 20) | (b.min(1023) << 10) | c.min(1023)
}

fn unpack(s: u32) -> (u32, u32, u32) {
    (s >> 20, (s >> 10) & 1023, s & 1023)
}

fn max_of(list: &[Selector]) -> (u32, u32, u32) {
    unpack(list.iter().map(Selector::specificity).max().unwrap_or(0))
}

fn compound_specificity(compound: &Compound) -> (u32, u32, u32) {
    let (mut a, mut b, mut c) = (0, 0, u32::from(compound.tag.is_some()));
    for simple in &compound.simple {
        let (x, y, z) = match simple {
            Simple::Id(_) => (1, 0, 0),
            Simple::Class(_) | Simple::Attr { .. } => (0, 1, 0),
            Simple::Pseudo(Pseudo::Where(_)) => (0, 0, 0),
            Simple::Pseudo(Pseudo::Not(list) | Pseudo::Is(list) | Pseudo::Has(list)) => max_of(list),
            Simple::Pseudo(Pseudo::Nth { of: Some(list), .. }) => {
                let (x, y, z) = max_of(list);
                (x, y + 1, z)
            }
            Simple::Pseudo(_) => (0, 1, 0),
        };
        a += x;
        b += y;
        c += z;
    }
    (a, b, c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_html;

    fn ids(doc: &Document, selector: &str) -> Vec<String> {
        query_all(doc, doc.document_id(), selector)
            .into_iter()
            .map(|n| doc.attr(n, "id").unwrap_or("?").to_string())
            .collect()
    }

    const PAGE: &str = "<body><div id='d' class='box big' data-x='Hello-World'>\
        <p id='p1'>a</p><p id='p2' class='note'>b</p><span id='s'>c</span><p id='p3'>d</p></div>\
        <ul id='u'><li id='l1'></li><li id='l2'>x</li><li id='l3'></li></ul>\
        <form id='f'><input id='i1' required><input id='i2' type='CHECKBOX' checked disabled>\
        <fieldset id='fs' disabled><input id='i3'></fieldset><textarea id='t' placeholder='x'></textarea></form>\
        <a id='a' href='/'>link</a><a id='na'>no link</a></body>";

    #[test]
    fn combinators() {
        let doc = parse_html(PAGE);
        assert_eq!(ids(&doc, "div > p"), ["p1", "p2", "p3"]);
        assert_eq!(ids(&doc, "#d p.note + span"), ["s"]);
        assert_eq!(ids(&doc, ".note ~ p"), ["p3"]);
        assert_eq!(ids(&doc, "body li"), ["l1", "l2", "l3"]);
        assert_eq!(ids(&doc, "ul>li:not(:empty)"), ["l2"]);
        assert_eq!(ids(&doc, "p, span#s"), ["p1", "p2", "s", "p3"]);
    }

    #[test]
    fn attributes() {
        let doc = parse_html(PAGE);
        assert_eq!(ids(&doc, "[data-x^='Hello']"), ["d"]);
        assert_eq!(ids(&doc, "[data-x|=hello i]"), ["d"]);
        assert_eq!(ids(&doc, "[class~=big]"), ["d"]);
        assert_eq!(ids(&doc, "[data-x*=o-W]"), ["d"]);
        assert_eq!(ids(&doc, "input[type=checkbox]"), ["i2"]);
    }

    #[test]
    fn structural_pseudo_classes() {
        let doc = parse_html(PAGE);
        assert_eq!(ids(&doc, "li:first-child"), ["l1"]);
        assert_eq!(ids(&doc, "li:last-child"), ["l3"]);
        assert_eq!(ids(&doc, "li:nth-child(2n+1)"), ["l1", "l3"]);
        assert_eq!(ids(&doc, "li:nth-child(even)"), ["l2"]);
        assert_eq!(ids(&doc, "#d > :nth-last-child(1)"), ["p3"]);
        assert_eq!(ids(&doc, "p:nth-of-type(2)"), ["p2"]);
        assert_eq!(ids(&doc, "#d > :nth-child(2 of p)"), ["p2"]);
        assert_eq!(ids(&doc, "span:only-of-type"), ["s"]);
        assert_eq!(ids(&doc, "div:has(> span)"), ["d"]);
        assert_eq!(ids(&doc, "p:has(+ span)"), ["p2"]);
        assert_eq!(ids(&doc, ":is(ul, form) > :where(li:first-child, textarea)"), ["l1", "t"]);
    }

    #[test]
    fn state_pseudo_classes() {
        let doc = parse_html(PAGE);
        assert_eq!(ids(&doc, ":checked"), ["i2"]);
        assert_eq!(ids(&doc, "input:disabled"), ["i2", "i3"]);
        assert_eq!(ids(&doc, "input:enabled"), ["i1"]);
        assert_eq!(ids(&doc, ":required"), ["i1"]);
        assert_eq!(ids(&doc, ":placeholder-shown"), ["t"]);
        assert_eq!(ids(&doc, "a:any-link"), ["a"]);
        assert!(ids(&doc, "a:hover").is_empty());
    }

    #[test]
    fn pseudo_elements_and_garbage_match_nothing() {
        let doc = parse_html(PAGE);
        assert!(ids(&doc, "p::before").is_empty());
        assert!(ids(&doc, "p:after").is_empty());
        assert!(ids(&doc, "p >").is_empty());
        assert!(ids(&doc, "[unclosed").is_empty());
        assert!(parse_list("div::before").unwrap()[0].pseudo_element);
    }

    #[test]
    fn specificity_follows_the_specification() {
        let s = |text: &str| unpack(parse_list(text).unwrap()[0].specificity());
        assert_eq!(s("*"), (0, 0, 0));
        assert_eq!(s("li"), (0, 0, 1));
        assert_eq!(s("ul li.x"), (0, 1, 2));
        assert_eq!(s("#a .b:hover"), (1, 2, 0));
        assert_eq!(s(":not(#a, .b)"), (1, 0, 0));
        assert_eq!(s(":where(#a) p"), (0, 0, 1));
        assert_eq!(s("a::before"), (0, 0, 2));
        assert_eq!(s("li:nth-child(2 of .x)"), (0, 2, 1));
    }

    #[test]
    fn escapes_and_odd_idents() {
        let doc = parse_html("<div id='a:b' class='md:flex -x'></div>");
        assert_eq!(query_all(&doc, doc.document_id(), "#a\\:b").len(), 1);
        assert_eq!(query_all(&doc, doc.document_id(), ".md\\:flex.-x").len(), 1);
    }
}
