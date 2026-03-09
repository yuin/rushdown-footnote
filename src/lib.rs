#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

use core::any::TypeId;
use core::cell::RefCell;
use core::fmt;
use core::fmt::Write;

use rushdown::{
    as_extension_data, as_extension_data_mut,
    ast::{pp_indent, Arena, KindData, NodeKind, NodeRef, NodeType, PrettyPrint, WalkStatus},
    context::{BoolValue, ContextKey, ContextKeyRegistry, ObjectValue},
    matches_kind,
    parser::{
        self, AnyBlockParser, AnyInlineParser, BlockParser, InlineParser, NoParserOptions, Parser,
        ParserExtension, ParserExtensionFn, PRIORITY_LINK, PRIORITY_LIST,
    },
    renderer::{
        self,
        html::{self, Renderer, RendererExtension, RendererExtensionFn},
        BoxRenderNode, NodeRenderer, NodeRendererRegistry, PostRender, Render, RenderNode,
        RendererOptions, TextWrite,
    },
    text::{self, Reader},
    util::{indent_position, is_blank},
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
    index: usize,
    references: Vec<usize>,
}

impl FootnoteDefinition {
    fn new(label: impl Into<text::Value>) -> Self {
        Self {
            label: label.into(),
            index: 0,
            references: Vec::new(),
        }
    }

    /// Returns the label of the footnote definition.
    #[inline(always)]
    fn label(&self) -> &text::Value {
        &self.label
    }

    /// Returns the index of the footnote definition.
    #[inline(always)]
    fn index(&self) -> usize {
        self.index
    }

    /// Returns the reference indices of the footnote definition.
    #[inline(always)]
    fn references(&self) -> &[usize] {
        &self.references
    }

    /// Adds a reference index to the footnote definition.
    #[inline(always)]
    fn add_reference(&mut self, ref_index: usize) {
        self.references.push(ref_index);
    }
}

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
        writeln!(w, "{}Label: {}", pp_indent(level), self.label.str(source))?;
        writeln!(w, "{}Index: {}", pp_indent(level), self.index,)?;
        writeln!(w, "{}References: {:?}", pp_indent(level), self.references())
    }
}

impl From<FootnoteDefinition> for KindData {
    fn from(e: FootnoteDefinition) -> Self {
        KindData::Extension(Box::new(e))
    }
}

// }}} AST

// Parser {{{

struct FootnoteDefinitions {
    definitions: Vec<NodeRef>,
    count: usize,
}

impl FootnoteDefinitions {
    fn new() -> Self {
        Self {
            definitions: Vec::new(),
            count: 0,
        }
    }
}

const FOOTNOTE_LIST: &str = "rushdown-footnote-l";
const REFERENCE_LIST: &str = "rushdown-footnote-r";
const FOOTNOTE_RENDER: &str = "rushdown-footnote-n";

#[derive(Debug)]
struct FootnoteDefinitionParser {
    footnote_list: ContextKey<ObjectValue>,
}

impl FootnoteDefinitionParser {
    /// Returns a new [`FootnoteDefinitionParser`].
    pub fn new(reg: Rc<RefCell<ContextKeyRegistry>>) -> Self {
        let footnote_list = reg.borrow_mut().get_or_create::<ObjectValue>(FOOTNOTE_LIST);
        Self { footnote_list }
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
        pos += 1; // skip the opening '['
        if !line.get(pos)?.eq(&b'^') {
            return None;
        }
        let open = pos + 1;
        let mut cur = open;
        let mut close = 0usize;
        while cur < line.len() {
            let c = line[cur];
            if c == b'\\' && line.get(cur + 1)? == &b']' {
                cur += 2;
                continue;
            }
            if c == b']' {
                close = cur;
                break;
            }
            cur += 1;
        }
        if close == 0 {
            return None;
        }
        if !line.get(close + 1)?.eq(&b':') {
            return None;
        }

        let label = text::Segment::new(
            seg.start() + open - seg.padding(),
            seg.start() + close - seg.padding(),
        );

        if label.is_blank(reader.source()) {
            return None;
        }

        let node = arena.new_node(FootnoteDefinition::new(label));
        reader.advance(close + 2);

        Some((node, parser::State::HAS_CHILDREN))
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
        _arena: &mut Arena,
        node_ref: NodeRef,
        _reader: &mut text::BasicReader,
        ctx: &mut parser::Context,
    ) {
        let mut list_opt = ctx.get_mut(self.footnote_list);
        if list_opt.is_none() {
            let lst = FootnoteDefinitions::new();
            ctx.insert(self.footnote_list, Box::new(lst));
            list_opt = ctx.get_mut(self.footnote_list);
        }
        let list = list_opt
            .unwrap()
            .downcast_mut::<FootnoteDefinitions>()
            .expect("Failed to downcast footnote list");
        list.definitions.push(node_ref);
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
    footnote_list: ContextKey<ObjectValue>,
    reference_list: ContextKey<ObjectValue>,
}

impl FootnoteReferenceParser {
    /// Returns a new [`FootnoteReferenceParser`].
    pub fn new(reg: Rc<RefCell<ContextKeyRegistry>>) -> Self {
        let footnote_list = reg.borrow_mut().get_or_create::<ObjectValue>(FOOTNOTE_LIST);
        let reference_list = reg
            .borrow_mut()
            .get_or_create::<ObjectValue>(REFERENCE_LIST);
        Self {
            footnote_list,
            reference_list,
        }
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
        let open = pos + 1;
        let mut cur = open;
        let mut close = 0usize;
        while cur < line.len() {
            let c = line[cur];
            if c == b'\\' && line.get(cur + 1)? == &b']' {
                cur += 2;
                continue;
            }
            if c == b']' {
                close = cur;
                break;
            }
            cur += 1;
        }
        if close == 0 {
            return None;
        }
        let label = text::Segment::new(seg.start() + open, seg.start() + close);

        let ref_index = {
            let list = if let Some(list) = ctx.get_mut(self.reference_list) {
                list
            } else {
                ctx.insert(self.reference_list, Box::new(Vec::<NodeRef>::new()));
                ctx.get_mut(self.reference_list).unwrap()
            }
            .downcast_mut::<Vec<NodeRef>>()
            .expect("Failed to downcast reference list");
            list.len() + 1
        };

        let list = ctx.get_mut(self.footnote_list).map(|v| {
            v.downcast_mut::<FootnoteDefinitions>()
                .expect("Failed to downcast footnote list")
        });
        if let Some(list) = list {
            let mut index = 0;
            for def_ref in &list.definitions {
                let def_data = as_extension_data_mut!(arena, *def_ref, FootnoteDefinition);
                if def_data.label().str(reader.source()) == label.str(reader.source()) {
                    if def_data.index() < 1 {
                        list.count += 1;
                        def_data.index = list.count;
                    }
                    index = def_data.index();
                    def_data.add_reference(ref_index);
                    break;
                }
            }
            if index == 0 {
                return None;
            }

            let list = ctx
                .get_mut(self.reference_list)
                .unwrap()
                .downcast_mut::<Vec<NodeRef>>()
                .expect("Failed to downcast reference list");

            let node = arena.new_node(FootnoteReference::new(label, index, ref_index));
            list.push(node);

            reader.advance(close + 1);

            if line[0] == b'!' {
                parent_ref
                    .merge_or_append_text_segment(arena, (seg.start(), seg.start() + 1).into());
            }
            return Some(node);
        }

        None
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

struct FootnoteReferenceHtmlRenderer<W: TextWrite> {
    _phantom: core::marker::PhantomData<W>,
    options: FootnoteHtmlRendererOptions,
    writer: html::Writer,
}

impl<W: TextWrite> FootnoteReferenceHtmlRenderer<W> {
    fn new(
        _reg: Rc<RefCell<ContextKeyRegistry>>,
        html_opts: html::Options,
        options: FootnoteHtmlRendererOptions,
    ) -> Self {
        Self {
            _phantom: core::marker::PhantomData,
            options,
            writer: html::Writer::with_options(html_opts),
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
        let data = as_extension_data!(arena, node_ref, FootnoteReference);
        if entering {
            let prefix = self.options.id_prefix.get_id(arena, node_ref, ctx);
            self.writer.write_html(
                w,
                &format!(
                    "<sup id=\"{}fnref:{}\"><a href=\"#{}fn:{}\" class=\"{}\" role=\"doc-noteref\">{}</a></sup>",
                    prefix,
                    data.ref_index(),
                    prefix,
                    data.index(),
                    self.options.link_class,
                    data.index()
                ),
            )?;
        }
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
    _phantom: core::marker::PhantomData<W>,
    footnote_list: ContextKey<ObjectValue>,
    footnote_render: ContextKey<BoolValue>,
}

impl<W: TextWrite> FootnoteDefinitionHtmlRenderer<W> {
    pub fn new(reg: Rc<RefCell<ContextKeyRegistry>>) -> Self {
        let footnote_list = reg.borrow_mut().get_or_create::<ObjectValue>(FOOTNOTE_LIST);
        let footnote_render = reg.borrow_mut().get_or_create::<BoolValue>(FOOTNOTE_RENDER);
        Self {
            _phantom: core::marker::PhantomData,
            footnote_list,
            footnote_render,
        }
    }
}

impl<W: TextWrite> RenderNode<W> for FootnoteDefinitionHtmlRenderer<W> {
    fn render_node<'a>(
        &self,
        _w: &mut W,
        _source: &'a str,
        _arena: &'a Arena,
        node_ref: NodeRef,
        entering: bool,
        ctx: &mut renderer::Context,
    ) -> Result<WalkStatus> {
        // If the footnote render flag is set, it means we are currently rendering footnotes, so we
        // continue rendering the footnote definition as normal.
        if ctx.get(self.footnote_render).is_some() {
            return Ok(WalkStatus::Continue);
        }

        // If we are entering the footnote definition node, we add it to the footnote list in the
        // context.
        // This is necessary because we need to render the footnote definitions at the end of the
        // document, and we need to know which footnote definitions to render.
        if entering {
            let mut list_opt = ctx.get_mut(self.footnote_list);
            if list_opt.is_none() {
                let lst = FootnoteDefinitions::new();
                ctx.insert(self.footnote_list, Box::new(lst));
                list_opt = ctx.get_mut(self.footnote_list);
            }
            let list = list_opt
                .unwrap()
                .downcast_mut::<FootnoteDefinitions>()
                .expect("Failed to downcast footnote list");
            list.definitions.push(node_ref);
        }
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

struct FootnotePostRenderHook<W: TextWrite> {
    _phantom: core::marker::PhantomData<W>,
    writer: html::Writer,
    footnote_list: ContextKey<ObjectValue>,
    footnote_render: ContextKey<BoolValue>,
    html_opts: html::Options,
    options: FootnoteHtmlRendererOptions,
}

impl<W: TextWrite> FootnotePostRenderHook<W> {
    pub fn new(
        reg: Rc<RefCell<ContextKeyRegistry>>,
        html_opts: html::Options,
        options: FootnoteHtmlRendererOptions,
    ) -> Self {
        let footnote_list = reg.borrow_mut().get_or_create::<ObjectValue>(FOOTNOTE_LIST);
        let footnote_render = reg.borrow_mut().get_or_create::<BoolValue>(FOOTNOTE_RENDER);
        Self {
            _phantom: core::marker::PhantomData,
            writer: html::Writer::with_options(html_opts.clone()),
            options,
            footnote_list,
            footnote_render,
            html_opts,
        }
    }
}

impl<W: TextWrite> PostRender<W> for FootnotePostRenderHook<W> {
    fn post_render(
        &self,
        w: &mut W,
        source: &str,
        arena: &Arena,
        _node_ref: NodeRef,
        render: &dyn Render<W>,
        ctx: &mut renderer::Context,
    ) -> Result<()> {
        if let Some(list_any) = ctx.remove(self.footnote_list) {
            let mut list = list_any
                .downcast::<FootnoteDefinitions>()
                .expect("Failed to downcast footnote list");
            if list.definitions.is_empty()
                || list.definitions.iter().all(|r| {
                    as_extension_data!(arena[*r], FootnoteDefinition)
                        .references()
                        .is_empty()
                })
            {
                return Ok(());
            }

            ctx.insert(self.footnote_render, true);
            list.definitions.sort_by(|a, b| {
                let a_data = as_extension_data!(arena[*a], FootnoteDefinition);
                let b_data = as_extension_data!(arena[*b], FootnoteDefinition);
                let ref_a = a_data.references().first().unwrap_or(&usize::MAX);
                let ref_b = b_data.references().first().unwrap_or(&usize::MAX);
                ref_a.cmp(ref_b)
            });
            self.writer
                .write_html(w, r#"<div class="footnotes" role="doc-endnotes">"#)?;
            self.writer.write_newline(w)?;
            if self.html_opts.xhtml {
                self.writer.write_html(w, "<hr />\n")?;
            } else {
                self.writer.write_html(w, "<hr>\n")?;
            }
            self.writer.write_html(w, "<ol>\n")?;
            let prefix = self.options.id_prefix.get_id(arena, _node_ref, ctx);

            for def_ref in &list.definitions {
                let def_data = as_extension_data!(arena, *def_ref, FootnoteDefinition);
                self.writer.write_html(
                    w,
                    &format!("<li id=\"{}fn:{}\">\n", prefix, def_data.index()),
                )?;
                let mut last_is_paragraph = false;
                for c in arena[*def_ref].children(arena) {
                    if c == arena[*def_ref].last_child().unwrap()
                        && matches_kind!(arena[c], Paragraph)
                    {
                        last_is_paragraph = true;
                        break;
                    }
                    render.render(w, source, arena, c, ctx)?;
                }
                if last_is_paragraph {
                    let last_child = arena[*def_ref].last_child().unwrap();
                    self.writer.write_safe_str(w, "<p>")?;
                    for c in arena[last_child].children(arena) {
                        render.render(w, source, arena, c, ctx)?;
                    }
                }
                for ref_index in def_data.references() {
                    self.writer.write_html(
                            w,
                            &format!(
                                "&#160;<a href=\"#{}fnref:{}\" class=\"{}\" role=\"doc-backlink\">{}</a>",
                                prefix,
                                ref_index,
                                self.options.backlink_class,
                                self.options.backlink_html
                            ),
                        )?;
                }
                if last_is_paragraph {
                    self.writer.write_safe_str(w, "</p>\n")?;
                }
                self.writer.write_html(w, "</li>\n")?;
            }
            self.writer.write_html(w, "</ol>\n")?;
            self.writer.write_html(w, "</div>\n")?;
            ctx.remove(self.footnote_render);
        }
        Ok(())
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
        r.add_post_render_hook(FootnotePostRenderHook::new, options.clone(), 500);
        r.add_node_renderer(FootnoteDefinitionHtmlRenderer::new, options.clone());
        r.add_node_renderer(FootnoteReferenceHtmlRenderer::new, options);
    })
}

// }}}
