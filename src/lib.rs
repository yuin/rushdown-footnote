#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use core::any::TypeId;
use core::cell::RefCell;
use core::fmt;
use core::fmt::Write;
use core::marker::PhantomData;

use rushdown::{
    as_extension_data,
    ast::{
        pp_indent, Arena, InlineChars, KindData, NodeKind, NodeRef, NodeType, PrettyPrint, Text,
        WalkStatus,
    },
    context::{ContextKey, ContextKeyRegistry, ObjectValue},
    matches_extension_kind, matches_kind,
    parser::{
        self, AnyBlockParser, AnyInlineParser, BlockParser, InlineParser, NoParserOptions, Parser,
        ParserExtension, ParserExtensionFn, PRIORITY_LINK, PRIORITY_LIST,
    },
    renderer::{
        self,
        html::{self, Renderer, RendererExtension, RendererExtensionFn},
        BoxRenderNode, BoxRenderNodeDecorator, NextRenderNode, NoRendererOptions, NodeRenderer,
        NodeRendererRegistry, RenderNode, RenderNodeDecorator, RendererOptions, TextWrite,
    },
    text::{self, Reader},
    util::{indent_position, is_blank, AsciiWordSet},
    Result,
};

// AST {{{

/// A struct representing a footnote reference in the AST.
#[derive(Debug)]
pub struct FootnoteReference {
    label: text::Value,
    index: usize,
    ref_index: usize,
}

impl FootnoteReference {
    pub fn new(label: impl Into<text::Value>, index: usize, ref_index: usize) -> Self {
        Self {
            label: label.into(),
            index,
            ref_index,
        }
    }

    /// Returns the label of the footnote reference.
    #[inline(always)]
    pub fn label(&self) -> &text::Value {
        &self.label
    }

    /// Returns the index of the footnote definition.
    #[inline(always)]
    pub fn index(&self) -> usize {
        self.index
    }

    /// Returns the reference index of the footnote reference.
    #[inline(always)]
    pub fn ref_index(&self) -> usize {
        self.ref_index
    }
}

impl InlineChars for FootnoteReference {}

impl NodeKind for FootnoteReference {
    fn typ(&self) -> NodeType {
        NodeType::Inline
    }

    fn kind_name(&self) -> &'static str {
        "FootnoteReference"
    }
}

impl PrettyPrint for FootnoteReference {
    fn pretty_print(&self, w: &mut dyn Write, source: &str, level: usize) -> fmt::Result {
        writeln!(w, "{}Label: {}", pp_indent(level), self.label().str(source))?;
        writeln!(w, "{}Index: {}", pp_indent(level), self.index())?;
        writeln!(w, "{}RefIndex: {}", pp_indent(level), self.ref_index())
    }
}

impl From<FootnoteReference> for KindData {
    fn from(e: FootnoteReference) -> Self {
        KindData::Extension(Box::new(e))
    }
}

/// A struct representing a footnote definition in the AST.
#[derive(Debug)]
pub struct FootnoteDefinition {
    label: text::Value,
}

impl FootnoteDefinition {
    fn new(label: impl Into<text::Value>) -> Self {
        Self {
            label: label.into(),
        }
    }

    /// Returns the label of the footnote definition.
    #[inline(always)]
    pub fn label(&self) -> &text::Value {
        &self.label
    }
}

impl InlineChars for FootnoteDefinition {}

impl NodeKind for FootnoteDefinition {
    fn typ(&self) -> NodeType {
        NodeType::ContainerBlock
    }

    fn kind_name(&self) -> &'static str {
        "FootnoteDefinition"
    }
}

impl PrettyPrint for FootnoteDefinition {
    fn pretty_print(&self, w: &mut dyn Write, source: &str, level: usize) -> fmt::Result {
        writeln!(w, "{}Label: {}", pp_indent(level), self.label.str(source))
    }
}

impl From<FootnoteDefinition> for KindData {
    fn from(e: FootnoteDefinition) -> Self {
        KindData::Extension(Box::new(e))
    }
}

// }}} AST

// Parser {{{

const FOOTNOTES: &str = "rushdown-footnote-fns";
const FOOTNOTE_DEFS: &str = "rushdown-footnote-defs";

#[derive(Debug)]
struct DefData {
    index: usize,
    references: Vec<usize>,
}

#[derive(Debug)]
struct Footnotes {
    definition_index: usize,
    defs_by_label: BTreeMap<String, DefData>,
}

impl Footnotes {
    fn new() -> Self {
        Self {
            definition_index: 1,
            defs_by_label: BTreeMap::new(),
        }
    }

    fn add_definition(&mut self, label: String) {
        if self.defs_by_label.contains_key(&label) {
            return;
        }
        self.defs_by_label.insert(
            label,
            DefData {
                index: 0,
                references: Vec::new(),
            },
        );
    }

    fn add_reference(&mut self, label: &str) -> Option<(usize, usize)> {
        let data = self.defs_by_label.get_mut(label)?;
        if data.index == 0 {
            data.index = self.definition_index;
            self.definition_index += 1;
        }
        let ref_index = data.references.len();
        data.references.push(ref_index);
        Some((data.index, ref_index))
    }
}

fn footnotes(ctx: &mut parser::Context, key: ContextKey<ObjectValue>) -> &mut Footnotes {
    if ctx.get_mut(key).is_none() {
        ctx.insert(key, Box::new(Footnotes::new()));
    }
    ctx.get_mut(key)
        .unwrap()
        .downcast_mut::<Footnotes>()
        .expect("Failed to downcast footnotes")
}

fn find_label_closure(bs: &[u8]) -> isize {
    let mut i = 0;
    let lim = core::cmp::min(bs.len(), 999);
    while i < lim {
        let c = bs[i];
        if c == b'\\' && i + 1 < bs.len() && bs[i + 1] == b']' {
            i += 2;
            continue;
        }
        if c == b']' {
            return i as isize;
        }
        i += 1;
    }
    -1
}

#[derive(Debug)]
struct FootnoteDefinitionParser {
    footnotes: ContextKey<ObjectValue>,
}

impl FootnoteDefinitionParser {
    /// Returns a new [`FootnoteDefinitionParser`].
    pub fn new(reg: Rc<RefCell<ContextKeyRegistry>>) -> Self {
        let footnotes = reg.borrow_mut().get_or_create::<ObjectValue>(FOOTNOTES);
        Self { footnotes }
    }
}

impl BlockParser for FootnoteDefinitionParser {
    fn trigger(&self) -> &[u8] {
        b"["
    }

    fn open(
        &self,
        arena: &mut Arena,
        _parent_ref: NodeRef,
        reader: &mut text::BasicReader,
        ctx: &mut parser::Context,
    ) -> Option<(NodeRef, parser::State)> {
        let (line, seg) = reader.peek_line_bytes()?;
        let mut pos = ctx.block_offset()?;
        if line.get(pos)? != &b'[' {
            return None;
        }
        pos += 1;
        if pos > line.len() - 1 || line.get(pos)? != &b'^' {
            return None;
        }
        let open = pos + 1;
        let closure = find_label_closure(line.get(pos + 1..)?);
        if closure < 0 {
            return None;
        }
        let closes = pos + 1 + closure as usize;
        let next = closes + 1;
        if next >= line.len() || line.get(next)? != &b':' {
            return None;
        }
        let padding = seg.padding();
        let label = text::Value::from_index(
            text::Index::new(seg.start() + open - padding, seg.start() + closes - padding),
            reader.decoder(),
        );
        if is_blank(label.bytes(reader.source()).as_ref()) {
            return None;
        }
        let node_ref = arena.new_node(FootnoteDefinition::new(label));

        let pos = next + 1 - padding;
        if pos >= line.len() {
            reader.advance(pos);
            return Some((node_ref, parser::State::NO_CHILDREN));
        }
        reader.advance_and_set_padding(pos, padding);
        Some((node_ref, parser::State::HAS_CHILDREN))
    }

    fn cont(
        &self,
        _arena: &mut Arena,
        _node_ref: NodeRef,
        reader: &mut text::BasicReader,
        _ctx: &mut parser::Context,
    ) -> Option<parser::State> {
        let (line, _) = reader.peek_line_bytes()?;
        if is_blank(&line) {
            return Some(parser::State::HAS_CHILDREN);
        }
        let (childpos, padding) = indent_position(&line, reader.line_offset(), 4)?;
        reader.advance_and_set_padding(childpos, padding);
        Some(parser::State::HAS_CHILDREN)
    }

    fn close(
        &self,
        arena: &mut Arena,
        node_ref: NodeRef,
        reader: &mut text::BasicReader,
        ctx: &mut parser::Context,
    ) {
        let label = as_extension_data!(arena, node_ref, FootnoteDefinition)
            .label()
            .str(reader.source())
            .into_owned();
        footnotes(ctx, self.footnotes).add_definition(label);
    }

    fn can_interrupt_paragraph(&self) -> bool {
        true
    }
}

impl From<FootnoteDefinitionParser> for AnyBlockParser {
    fn from(p: FootnoteDefinitionParser) -> Self {
        AnyBlockParser::Extension(Box::new(p))
    }
}

#[derive(Debug)]
struct FootnoteReferenceParser {
    footnotes: ContextKey<ObjectValue>,
}

impl FootnoteReferenceParser {
    /// Returns a new [`FootnoteReferenceParser`].
    pub fn new(reg: Rc<RefCell<ContextKeyRegistry>>) -> Self {
        let footnotes = reg.borrow_mut().get_or_create::<ObjectValue>(FOOTNOTES);
        Self { footnotes }
    }
}

impl InlineParser for FootnoteReferenceParser {
    fn trigger(&self) -> &[u8] {
        // footnote syntax probably conflict with the image syntax.
        // So we need trigger this parser with '!'.
        b"!["
    }

    fn parse(
        &self,
        arena: &mut Arena,
        parent_ref: NodeRef,
        reader: &mut text::BlockReader,
        ctx: &mut parser::Context,
    ) -> Option<NodeRef> {
        let (line, seg) = reader.peek_line_bytes()?;
        let mut pos = 1;
        if line.first()? == &b'!' {
            pos += 1;
        }
        if line.get(pos)? != &b'^' {
            return None;
        }
        pos += 1;
        let open = pos;
        let closure = find_label_closure(line.get(pos..)?);
        if closure < 0 {
            return None;
        }
        let closes = pos + closure as usize;
        let label = text::Value::from_index(
            text::Index::new(seg.start() + open, seg.start() + closes),
            reader.decoder(),
        );
        let label_str = label.str(reader.source()).into_owned();
        let (index, ref_index) = footnotes(ctx, self.footnotes).add_reference(&label_str)?;
        reader.advance(closes + 1);
        if line[0] == b'!' {
            let text_ref = arena.new_node(Text::new(text::Value::from_index(
                text::Index::new(seg.start(), seg.start() + 1),
                reader.decoder(),
            )));
            parent_ref.append_child(arena, text_ref);
        }
        Some(arena.new_node(FootnoteReference::new(label, index, ref_index)))
    }
}

impl From<FootnoteReferenceParser> for AnyInlineParser {
    fn from(p: FootnoteReferenceParser) -> Self {
        AnyInlineParser::Extension(Box::new(p))
    }
}

// }}}

// Renderer {{{

/// An enum representing the prefix of footnote IDs.
#[derive(Debug, Clone)]
pub enum FootnoteIdPrefix {
    None,
    Value(String),
    Function(fn(&Arena, NodeRef, &renderer::Context) -> String),
}

impl FootnoteIdPrefix {
    pub fn get_id(
        &self,
        arena: &Arena,
        node_ref: NodeRef,
        ctx: &renderer::Context,
    ) -> Cow<'static, str> {
        match self {
            FootnoteIdPrefix::None => Cow::Borrowed(""),
            FootnoteIdPrefix::Value(prefix) => Cow::Owned(prefix.clone()),
            FootnoteIdPrefix::Function(f) => Cow::Owned(f(arena, node_ref, ctx)),
        }
    }
}

/// Options for the footnote HTML renderer.
#[derive(Debug, Clone)]
pub struct FootnoteHtmlRendererOptions {
    /// The class name for the footnote reference link.
    ///
    /// This defaults to "footnote-ref".
    pub link_class: String,

    /// The class name for the footnote backlink.
    ///
    /// This defaults to "footnote-backref".
    pub backlink_class: String,

    /// The HTML content for the footnote backlink.
    /// This defaults to "&#x21a9;&#xfe0e;" (the leftwards arrow with hook character).
    pub backlink_html: String,

    /// The prefix for footnote IDs.
    pub id_prefix: FootnoteIdPrefix,
}

impl Default for FootnoteHtmlRendererOptions {
    fn default() -> Self {
        Self {
            link_class: "footnote-ref".to_string(),
            backlink_class: "footnote-backref".to_string(),
            backlink_html: "&#x21a9;&#xfe0e;".to_string(),
            id_prefix: FootnoteIdPrefix::None,
        }
    }
}

impl RendererOptions for FootnoteHtmlRendererOptions {}

#[derive(Debug, Default)]
struct DefInfo {
    index: usize,
    references: Vec<usize>,
}

#[derive(Debug, Default)]
struct CollectedFootnotes {
    defs: BTreeMap<String, NodeRef>,
    infos: BTreeMap<String, DefInfo>,
}

fn collect_footnotes(
    arena: &Arena,
    root_ref: NodeRef,
    source: &str,
    footnotes: &mut CollectedFootnotes,
) {
    if matches_extension_kind!(arena, root_ref, FootnoteDefinition) {
        let label = as_extension_data!(arena, root_ref, FootnoteDefinition)
            .label()
            .str(source)
            .into_owned();
        footnotes.defs.insert(label.clone(), root_ref);
        footnotes.infos.entry(label).or_default();
    } else if matches_extension_kind!(arena, root_ref, FootnoteReference) {
        let data = as_extension_data!(arena, root_ref, FootnoteReference);
        let label = data.label().str(source).into_owned();
        let info = footnotes.infos.entry(label).or_default();
        if info.index == 0 {
            info.index = data.index();
        }
        info.references.push(data.ref_index());
    }
    for child in arena[root_ref].children(arena) {
        collect_footnotes(arena, child, source, footnotes);
    }
}

struct RenderCtx<'a, W: TextWrite> {
    w: &'a mut W,
    source: &'a str,
    arena: &'a Arena,
    next: &'a NextRenderNode<'a, W>,
    context: &'a mut renderer::Context,
}

fn render_tree<W: TextWrite>(cache: &mut RenderCtx<'_, W>, node_ref: NodeRef) -> Result<()> {
    let context = &mut *cache.context;
    let status =
        cache
            .next
            .render_node(cache.w, cache.source, cache.arena, node_ref, true, context)?;
    if status != WalkStatus::Continue {
        return Ok(());
    }
    let children: Vec<NodeRef> = cache.arena[node_ref].children(cache.arena).collect();
    for child in children {
        render_tree(cache, child)?;
    }
    cache.next.render_node(
        cache.w,
        cache.source,
        cache.arena,
        node_ref,
        false,
        cache.context,
    )?;
    Ok(())
}

struct FootnoteDecorator<W: TextWrite> {
    _phantom: PhantomData<W>,
    options: FootnoteHtmlRendererOptions,
    xhtml: bool,
    defs_key: ContextKey<ObjectValue>,
    attr_filter: AsciiWordSet,
}

impl<W: TextWrite> FootnoteDecorator<W> {
    fn new(
        reg: Rc<RefCell<ContextKeyRegistry>>,
        html_opts: html::Options,
        options: FootnoteHtmlRendererOptions,
    ) -> Self {
        let defs_key = reg.borrow_mut().get_or_create::<ObjectValue>(FOOTNOTE_DEFS);
        Self {
            _phantom: PhantomData,
            options,
            xhtml: html_opts.xhtml,
            defs_key,
            attr_filter: AsciiWordSet::new(html::LIST_ITEM_ATTRS),
        }
    }

    fn render_definitions(
        &self,
        cache: &mut RenderCtx<'_, W>,
        footnotes: &CollectedFootnotes,
    ) -> Result<()> {
        let mut referenced: Vec<(usize, NodeRef, &DefInfo)> = footnotes
            .infos
            .iter()
            .filter(|(_, info)| !info.references.is_empty())
            .filter_map(|(label, info)| {
                footnotes
                    .defs
                    .get(label)
                    .map(|def_ref| (info.index, *def_ref, info))
            })
            .collect();
        if referenced.is_empty() {
            return Ok(());
        }
        referenced.sort_by_key(|(index, _, _)| *index);

        cache
            .w
            .write_str("<div class=\"footnotes\" role=\"doc-endnotes\">\n")?;
        if self.xhtml {
            cache.w.write_str("<hr />\n")?;
        } else {
            cache.w.write_str("<hr>\n")?;
        }
        cache.w.write_str("<ol>\n")?;
        for (index, def_ref, info) in referenced {
            self.render_definition(cache, index, def_ref, info)?;
        }
        cache.w.write_str("</ol>\n")?;
        cache.w.write_str("</div>\n")
    }

    fn render_definition(
        &self,
        cache: &mut RenderCtx<'_, W>,
        index: usize,
        def_ref: NodeRef,
        info: &DefInfo,
    ) -> Result<()> {
        let prefix = self
            .options
            .id_prefix
            .get_id(cache.arena, def_ref, cache.context);
        cache
            .w
            .write_str(&format!("<li id=\"{}fn:{}\"", prefix, index))?;
        html::render_attributes(
            cache.w,
            cache.source,
            cache.arena[def_ref].attributes(),
            Some(&self.attr_filter),
        )?;
        cache.w.write_str(">\n")?;

        let last_para = cache.arena[def_ref]
            .last_child()
            .filter(|last| matches_kind!(cache.arena, *last, Paragraph));
        let children: Vec<NodeRef> = cache.arena[def_ref].children(cache.arena).collect();
        for child in children {
            if Some(child) == last_para {
                self.render_paragraph_with_backlinks(cache, child, info)?;
            } else {
                render_tree(cache, child)?;
            }
        }
        if last_para.is_none() {
            self.render_backlinks(cache, def_ref, info)?;
        }
        cache.w.write_str("</li>\n")
    }

    fn render_paragraph_with_backlinks(
        &self,
        cache: &mut RenderCtx<'_, W>,
        para_ref: NodeRef,
        info: &DefInfo,
    ) -> Result<()> {
        cache.w.write_str("<p>")?;
        let children: Vec<NodeRef> = cache.arena[para_ref].children(cache.arena).collect();
        for child in children {
            render_tree(cache, child)?;
        }
        self.render_backlinks(cache, para_ref, info)?;
        cache.w.write_str("</p>\n")
    }

    fn render_backlinks(
        &self,
        cache: &mut RenderCtx<'_, W>,
        node_ref: NodeRef,
        info: &DefInfo,
    ) -> Result<()> {
        let prefix = self
            .options
            .id_prefix
            .get_id(cache.arena, node_ref, cache.context);
        for ref_index in &info.references {
            cache.w.write_str("&#160;<a href=\"#")?;
            cache.w.write_str(&prefix)?;
            cache.w.write_str("fnref")?;
            if *ref_index > 0 {
                cache.w.write_str(&format!("{}", ref_index))?;
            }
            cache.w.write_str(&format!(
                ":{}\" class=\"{}\" role=\"doc-backlink\">{}</a>",
                info.index, self.options.backlink_class, self.options.backlink_html
            ))?;
        }
        Ok(())
    }
}

impl<W: TextWrite> RenderNodeDecorator<W> for FootnoteDecorator<W> {
    fn decorate_node<'a>(
        &self,
        next: &NextRenderNode<'_, W>,
        w: &mut W,
        source: &'a str,
        arena: &'a Arena,
        node_ref: NodeRef,
        entering: bool,
        ctx: &mut renderer::Context,
    ) -> Result<WalkStatus> {
        if entering {
            let mut footnotes = CollectedFootnotes::default();
            collect_footnotes(arena, node_ref, source, &mut footnotes);
            ctx.insert(self.defs_key, Box::new(footnotes));
            return next.render_node(w, source, arena, node_ref, entering, ctx);
        }

        let Some(raw) = ctx.remove(self.defs_key) else {
            return next.render_node(w, source, arena, node_ref, entering, ctx);
        };
        let footnotes = raw
            .downcast::<CollectedFootnotes>()
            .expect("Failed to downcast footnote definitions");
        self.render_definitions(
            &mut RenderCtx {
                w,
                source,
                arena,
                next,
                context: ctx,
            },
            &footnotes,
        )?;
        next.render_node(w, source, arena, node_ref, entering, ctx)
    }
}

impl<'cb, W> NodeRenderer<'cb, W> for FootnoteDecorator<W>
where
    W: TextWrite + 'cb,
{
    fn register_node_renderer_fn(self, nrr: &mut impl NodeRendererRegistry<'cb, W>) {
        nrr.register_node_renderer_decorator_fn(
            TypeId::of::<rushdown::ast::Document>(),
            BoxRenderNodeDecorator::new(self),
        );
    }
}

struct FootnoteReferenceHtmlRenderer<W: TextWrite> {
    _phantom: PhantomData<W>,
    options: FootnoteHtmlRendererOptions,
}

impl<W: TextWrite> FootnoteReferenceHtmlRenderer<W> {
    fn new(options: FootnoteHtmlRendererOptions) -> Self {
        Self {
            _phantom: PhantomData,
            options,
        }
    }
}

impl<W: TextWrite> RenderNode<W> for FootnoteReferenceHtmlRenderer<W> {
    fn render_node<'a>(
        &self,
        w: &mut W,
        _source: &'a str,
        arena: &'a Arena,
        node_ref: NodeRef,
        entering: bool,
        ctx: &mut renderer::Context,
    ) -> Result<WalkStatus> {
        if !entering {
            return Ok(WalkStatus::Continue);
        }
        let data = as_extension_data!(arena, node_ref, FootnoteReference);
        let prefix = self.options.id_prefix.get_id(arena, node_ref, ctx);
        w.write_str("<sup id=\"")?;
        w.write_str(&prefix)?;
        w.write_str("fnref")?;
        if data.ref_index() > 0 {
            w.write_str(&format!("{}", data.ref_index()))?;
        }
        w.write_str(&format!(
            ":{}\"><a href=\"#{}fn:{}\" class=\"{}\" role=\"doc-noteref\">{}</a></sup>",
            data.index(),
            prefix,
            data.index(),
            self.options.link_class,
            data.index()
        ))?;
        Ok(WalkStatus::SkipChildren)
    }
}

impl<'cb, W> NodeRenderer<'cb, W> for FootnoteReferenceHtmlRenderer<W>
where
    W: TextWrite + 'cb,
{
    fn register_node_renderer_fn(self, nrr: &mut impl NodeRendererRegistry<'cb, W>) {
        nrr.register_node_renderer_fn(TypeId::of::<FootnoteReference>(), BoxRenderNode::new(self));
    }
}

struct FootnoteDefinitionHtmlRenderer<W: TextWrite> {
    _phantom: PhantomData<W>,
}

impl<W: TextWrite> FootnoteDefinitionHtmlRenderer<W> {
    fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<W: TextWrite> RenderNode<W> for FootnoteDefinitionHtmlRenderer<W> {
    fn render_node<'a>(
        &self,
        _w: &mut W,
        _source: &'a str,
        _arena: &'a Arena,
        _node_ref: NodeRef,
        _entering: bool,
        _ctx: &mut renderer::Context,
    ) -> Result<WalkStatus> {
        Ok(WalkStatus::SkipChildren)
    }
}

impl<'cb, W> NodeRenderer<'cb, W> for FootnoteDefinitionHtmlRenderer<W>
where
    W: TextWrite + 'cb,
{
    fn register_node_renderer_fn(self, nrr: &mut impl NodeRendererRegistry<'cb, W>) {
        nrr.register_node_renderer_fn(TypeId::of::<FootnoteDefinition>(), BoxRenderNode::new(self));
    }
}

// }}} Renderer

// Extension {{{

/// Returns a parser extension that parses footnotes.
pub fn footnote_parser_extension() -> impl ParserExtension {
    ParserExtensionFn::new(|p: &mut Parser| {
        p.add_inline_parser(
            FootnoteReferenceParser::new,
            NoParserOptions,
            PRIORITY_LINK - 100,
        );
        p.add_block_parser(
            FootnoteDefinitionParser::new,
            NoParserOptions,
            PRIORITY_LIST + 100,
        );
    })
}

/// Returns a renderer extension that renders footnotes in HTML.
pub fn footnote_html_renderer_extension<'cb, W>(
    options: impl Into<FootnoteHtmlRendererOptions>,
) -> impl RendererExtension<'cb, W>
where
    W: TextWrite + 'cb,
{
    RendererExtensionFn::new(move |r: &mut Renderer<'cb, W>| {
        let options = options.into();
        r.add_node_renderer(FootnoteDecorator::new, options.clone());
        r.add_node_renderer(FootnoteDefinitionHtmlRenderer::new, NoRendererOptions);
        r.add_node_renderer(FootnoteReferenceHtmlRenderer::new, options);
    })
}

// }}}
