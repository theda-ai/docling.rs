//! Reader for the `index.apxl` of an iWork '09 (and earlier) Keynote document
//! (docling's `docling/backend/iwork/keynote_xml.py`, docling#4330; #466).
//!
//! Keynote wrote a plain XML tree before 2013, in the `key` namespace over
//! the `sf` one every iWork app shared, and it describes the same presentation
//! the modern container keeps in an object graph: a slide list, and a slide
//! holding a title placeholder, a body placeholder, presenter notes and a page
//! whose layers hold everything else. Only the elements Keynote adds live
//! here; the `sf` vocabulary underneath — runs, character and list styles,
//! tables, placed images — is read by [`super::pages_xml`]'s helpers.

use std::collections::{HashMap, HashSet};

use roxmltree::{Document, Node as XmlNode, NodeId};

use super::keynote::{
    reading_order, Geometry, Placed, Presentation, Slide, DEFAULT_SLIDE_HEIGHT, DEFAULT_SLIDE_WIDTH,
};
use super::ooxml::Package;
use super::pages::{Block, Comment, Formatting, Label, ListLabel, ListStyle, Paragraph};
use super::pages_xml::{
    float_attr, int_attr, is_media, is_sf, legacy_formatting, legacy_geometry,
    legacy_inherited_lists, legacy_list_styles, legacy_picture, legacy_runs, legacy_styles,
    legacy_table, read_index_xml, sfa_attr, SF_NS,
};
use crate::error::ConversionError;

const KEY_NS: &str = "http://developer.apple.com/namespaces/keynote2";

fn is_key(node: XmlNode, name: &str) -> bool {
    node.is_element()
        && node.tag_name().namespace() == Some(KEY_NS)
        && node.tag_name().name() == name
}

/// The style lookups a presentation's text is read against — built once for
/// the whole document, since a slide's text is styled from the theme rather
/// than from anything the slide itself holds.
struct Styles {
    characters: HashMap<String, Option<Formatting>>,
    lists: HashMap<String, ListStyle>,
    /// The list style each paragraph style ends up carrying (Keynote leaves
    /// `sf:list-style` off the paragraph and inherits it through the style).
    inherited: HashMap<String, String>,
}

/// docling's `read_content`: an iWork '09 presentation out of its
/// `index.apxl` (or `index.apxl.gz`).
pub(crate) fn read_content(
    pkg: &mut Package,
    member: &str,
) -> Result<Presentation, ConversionError> {
    let xml = read_index_xml(pkg, member, "Keynote")?;
    let dom = Document::parse(&xml)
        .map_err(|e| ConversionError::Parse(format!("iwork: could not parse '{member}': {e}")))?;
    let root = dom.root_element();
    let styles = Styles {
        characters: legacy_styles(root, "characterstyle", legacy_formatting),
        lists: legacy_list_styles(root),
        inherited: legacy_inherited_lists(root),
    };
    let slides = iter_slides(root)
        .into_iter()
        .map(|slide| read_slide(slide, pkg, &styles))
        .collect();
    let (width, height) = slide_size(root);
    Ok(Presentation {
        slides,
        width,
        height,
    })
}

/// `iter_slides`: the presentation's slides, in presentation order. The theme
/// keeps its master slides elsewhere under a tag of its own, so reading the
/// `key:slide-list` is enough to leave them out.
fn iter_slides<'a>(root: XmlNode<'a, 'a>) -> Vec<XmlNode<'a, 'a>> {
    root.children()
        .filter(|c| is_key(*c, "slide-list"))
        .flat_map(|list| list.children().filter(|s| is_key(*s, "slide")))
        .collect()
}

/// `slide_size`: the size the slides are laid out at, in points (`key:size`),
/// the pre-widescreen default when it cannot be read.
fn slide_size(root: XmlNode) -> (f64, f64) {
    for child in root.children().filter(|c| is_key(*c, "size")) {
        if let (Some(w), Some(h)) = (float_attr(child, "w"), float_attr(child, "h")) {
            if w > 0.0 && h > 0.0 {
                return (w, h);
            }
        }
    }
    (DEFAULT_SLIDE_WIDTH, DEFAULT_SLIDE_HEIGHT)
}

/// `read_slide`: one slide — what is placed on it, its comments and its notes.
fn read_slide(slide: XmlNode, pkg: &mut Package, styles: &Styles) -> Slide {
    let placed = slide_drawables(slide);
    let geometries: Vec<Option<Geometry>> = placed.iter().map(|(_, _, g)| *g).collect();
    let mut blocks = Vec::new();
    let mut comments = Vec::new();
    for position in reading_order(&geometries) {
        let (element, label, geometry) = placed[position];
        let (found, said) = drawable_blocks(element, label, pkg, styles);
        blocks.extend(found.into_iter().map(|block| Placed { block, geometry }));
        comments.extend(said);
    }
    Slide {
        blocks,
        notes: slide_notes(slide, styles),
        comments,
    }
}

/// `slide_drawables`: what is placed on a slide, with the label its text
/// takes if it is a placeholder and where it sits, in stored order. The
/// page's drawables refer back to the slide's placeholders rather than
/// holding them, and a slide sometimes leaves a placeholder out of that list
/// altogether, so the placeholders are added afterwards as well — the same
/// merge the 2013 reader makes between a slide's two accounts of them.
fn slide_drawables<'a>(
    slide: XmlNode<'a, 'a>,
) -> Vec<(XmlNode<'a, 'a>, Option<Label>, Option<Geometry>)> {
    let placeholders = slide_placeholders(slide);
    let mut placed = Vec::new();
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut add = |element: XmlNode<'a, 'a>, label: Option<Label>| {
        if seen.insert(element.id()) {
            placed.push((element, label, legacy_geometry(element)));
        }
    };
    for drawable in iter_drawables(slide) {
        if is_sf(drawable, "title-placeholder-ref") || is_sf(drawable, "body-placeholder-ref") {
            // A reference carries no geometry of its own, so the placeholder
            // it names is what is placed and what is positioned.
            if let Some((element, label)) =
                placeholders.get(sfa_attr(drawable, "IDREF").unwrap_or(""))
            {
                add(*element, Some(*label));
            }
            continue;
        }
        add(drawable, None);
    }
    for (element, label) in placeholders.values() {
        add(*element, Some(*label));
    }
    placed
}

/// `slide_placeholders`: the slide's title and body placeholders by the
/// identifier they answer to. Neither is labelled by its paragraph style —
/// Keynote names theme styles in the theme's language — so the placeholder
/// is what says which is which.
fn slide_placeholders<'a>(slide: XmlNode<'a, 'a>) -> HashMap<String, (XmlNode<'a, 'a>, Label)> {
    let mut found = HashMap::new();
    for child in slide.children() {
        let label = if is_key(child, "title-placeholder") {
            Label::Title
        } else if is_key(child, "body-placeholder") {
            Label::Text
        } else {
            continue;
        };
        if let Some(identifier) = sfa_attr(child, "ID").filter(|id| !id.is_empty()) {
            found.insert(identifier.to_string(), (child, label));
        }
    }
    found
}

/// `iter_drawables`: the drawables of a slide's own layers (`key:page` →
/// `sf:layers` → `sf:layer` → `sf:drawables`), leaving the master's alone —
/// the `sf:proxy-master-layer` is deliberately not followed, since what the
/// master draws belongs to every slide using it.
fn iter_drawables<'a>(slide: XmlNode<'a, 'a>) -> Vec<XmlNode<'a, 'a>> {
    slide
        .children()
        .filter(|c| is_key(*c, "page"))
        .flat_map(|page| page.children().filter(|c| is_sf(*c, "layers")))
        .flat_map(|layers| layers.children().filter(|c| is_sf(*c, "layer")))
        .flat_map(|layer| layer.children().filter(|c| is_sf(*c, "drawables")))
        .flat_map(|group| group.children().filter(XmlNode::is_element))
        .collect()
}

/// `drawable_blocks`: whichever kind of drawable is placed on a slide — the
/// blocks it contributes, and the comments it holds.
fn drawable_blocks(
    element: XmlNode,
    label: Option<Label>,
    pkg: &mut Package,
    styles: &Styles,
) -> (Vec<Block>, Vec<Comment>) {
    if is_sf(element, "sticky-note") {
        // A comment, drawn as a note stuck to the slide: no author is
        // recorded, so only its text is recovered.
        let text = sticky_note_text(element, styles);
        let comments = if text.is_empty() {
            Vec::new()
        } else {
            vec![Comment {
                text,
                anchor: String::new(),
            }]
        };
        return (Vec::new(), comments);
    }
    if is_sf(element, "tabular-info") {
        let table = element
            .descendants()
            .find(|n| is_sf(*n, "tabular-model"))
            .and_then(legacy_table);
        return (table.map(Block::Table).into_iter().collect(), Vec::new());
    }
    if is_media(element) {
        let picture = legacy_picture(element, pkg);
        return (
            picture.map(Block::Picture).into_iter().collect(),
            Vec::new(),
        );
    }
    let paragraphs = element_paragraphs(element, label.unwrap_or(Label::Text), styles);
    (
        paragraphs.into_iter().map(Block::Paragraph).collect(),
        Vec::new(),
    )
}

/// `element_paragraphs`: the non-empty paragraphs of a placeholder, a text
/// box or the presenter notes, every one given `label`.
fn element_paragraphs(element: XmlNode, label: Label, styles: &Styles) -> Vec<Paragraph> {
    element
        .descendants()
        .filter(|n| is_sf(*n, "p"))
        .filter_map(|para| {
            let runs = legacy_runs(para, &styles.characters);
            if runs.is_empty() {
                return None;
            }
            Some(Paragraph {
                runs,
                label,
                list: list_label(para, styles),
                anchors: Vec::new(),
            })
        })
        .collect()
}

/// docling's `legacy_list_label` with the inherited lookup: how an '09
/// paragraph is labelled as a list item, if it is one. A paragraph that names
/// no `sf:list-style` takes the one its paragraph style carries.
/// `sf:list-level` is the rung of the style's ladder, counted from the
/// unlabelled one ordinary body text sits on (a paragraph naming no level is
/// on that rung); docling counts nesting from the first labelled rung
/// instead, so the depth is one less.
fn list_label(paragraph: XmlNode, styles: &Styles) -> Option<ListLabel> {
    let named = paragraph.attribute((SF_NS, "list-style")).or_else(|| {
        styles
            .inherited
            .get(paragraph.attribute((SF_NS, "style")).unwrap_or(""))
            .map(String::as_str)
    })?;
    let style = styles.lists.get(named)?;
    let rung = int_attr(paragraph, "list-level").unwrap_or(0);
    let mut label = style.label(rung)?;
    label.depth = rung.saturating_sub(1);
    Some(label)
}

/// `sticky_note_text`: the text of a sticky note, its paragraphs run together.
fn sticky_note_text(element: XmlNode, styles: &Styles) -> String {
    element
        .descendants()
        .filter(|n| is_sf(*n, "p"))
        .map(|para| {
            legacy_runs(para, &styles.characters)
                .into_iter()
                .map(|r| r.text)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// `slide_notes`: the presenter notes of one slide (`key:notes`).
fn slide_notes(slide: XmlNode, styles: &Styles) -> Vec<Paragraph> {
    slide
        .children()
        .filter(|c| is_key(*c, "notes"))
        .flat_map(|notes| element_paragraphs(notes, Label::Text, styles))
        .collect()
}
