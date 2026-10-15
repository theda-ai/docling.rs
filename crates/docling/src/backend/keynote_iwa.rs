//! Reader for the object graph of a Keynote 6+ (2013 onwards) presentation
//! (docling's `docling/backend/iwork/keynote_iwa.py`, docling#4330; #466).
//!
//! The container is a set of `Index/*.iwa` archives, the same ones a Pages
//! document is written into, so everything below a slide — text storages,
//! tables, images — is read by [`super::pages_iwa`]'s reader. What `KN`,
//! Keynote's own namespace, adds on top is a show holding a tree of slide
//! nodes, and a slide holding placeholders: a title, a body, a slide number,
//! and whatever else was dropped onto it.
//!
//! A placeholder is what makes a slide title recoverable. Its *style* is no
//! help: the style names are localised (one of the fixtures names them in
//! Indonesian), so the title is taken from the placeholder Keynote reserves
//! for it rather than from a style called "Title". Apple publishes no schema;
//! the message and field numbers below are upstream's.

use std::collections::HashSet;

use super::iwork::{first_bytes, Archive};
use super::keynote::{
    reading_order, titled, Geometry, Placed, Presentation, Slide, DEFAULT_SLIDE_HEIGHT,
    DEFAULT_SLIDE_WIDTH,
};
use super::ooxml::Package;
use super::pages::{Block, Comment, Paragraph};
use super::pages_iwa::{
    drawable_geometry, index_objects, read_point, reference_field, reference_list, Reader,
    SHAPE_STORAGE_FIELD, TSWP_STORAGE_ARCHIVE,
};
use crate::error::ConversionError;

/// `KN.DocumentArchive`, the root object of a presentation.
const KN_DOCUMENT_ARCHIVE: u32 = 1;
/// `KN.ShowArchive`, which holds the slides and their size.
const KN_SHOW_ARCHIVE: u32 = 2;
/// `KN.SlideNodeArchive`, one entry of the slide tree — the tree is what lets
/// a slide be grouped under another in the navigator.
const KN_SLIDE_NODE_ARCHIVE: u32 = 4;
/// `KN.SlideArchive`, one slide.
const KN_SLIDE_ARCHIVE: u32 = 5;
/// `KN.PlaceholderArchive`, one of a slide's reserved shapes. It wraps a
/// `TSWP.ShapeInfoArchive` rather than subclassing the drawable, so its text
/// is reached through that super.
const KN_PLACEHOLDER_ARCHIVE: u32 = 7;
/// `KN.NoteArchive`, the presenter notes of one slide.
const KN_NOTE_ARCHIVE: u32 = 15;
/// The shape a Keynote comment is drawn in: a sticky note in the slide's own
/// drawables list, holding a copy of the text for drawing and pointing at the
/// `TSD.CommentStorageArchive` that holds the comment proper (author,
/// replies) — read as a comment and never as body text.
const KN_COMMENT_ARCHIVE: u32 = 2014;

/// `KN.DocumentArchive` → its `KN.ShowArchive`.
const DOCUMENT_SHOW_FIELD: u32 = 2;
/// `KN.ShowArchive`'s slide tree — a message whose own repeated field points
/// at the slide nodes. The theme keeps a second list of the same shape
/// holding the master slides, which is why slides are reached from the show
/// and not by collecting every `KN.SlideArchive` in the container.
const SHOW_SLIDE_TREE_FIELD: u32 = 3;
/// `KN.ShowArchive`'s slide size, a `TSP.Size`.
const SHOW_SLIDE_SIZE_FIELD: u32 = 4;
/// The slide tree's nodes, in presentation order.
const SLIDE_TREE_NODES_FIELD: u32 = 2;
/// `KN.SlideNodeArchive` → the slide it stands for.
const SLIDE_NODE_SLIDE_FIELD: u32 = 2;
/// `KN.SlideArchive`'s title and body placeholders — written whether or not
/// the author typed into them, so an empty one yields nothing.
const SLIDE_TITLE_FIELD: u32 = 5;
const SLIDE_BODY_FIELD: u32 = 6;
/// Everything placed on the slide. It repeats the placeholders the slide
/// names separately, and sometimes omits one of them, so the two sources are
/// merged rather than either trusted alone.
const SLIDE_DRAWABLES_FIELD: u32 = 7;
/// The slide-number placeholder: page furniture holding nothing but the
/// U+FFFC the number is substituted for, so it is left out.
const SLIDE_NUMBER_FIELD: u32 = 20;
/// `KN.SlideArchive` → its `KN.NoteArchive`.
const SLIDE_NOTE_FIELD: u32 = 27;
/// `KN.PlaceholderArchive`'s embedded `TSWP.ShapeInfoArchive`.
const PLACEHOLDER_SHAPE_FIELD: u32 = 1;
/// `KN.NoteArchive` → the storage holding the notes.
const NOTE_TEXT_FIELD: u32 = 1;
/// A comment shape → its `TSD.CommentStorageArchive`.
const COMMENT_STORAGE_FIELD: u32 = 2;

fn fail(msg: &str) -> ConversionError {
    ConversionError::Parse(format!("iwork: {msg}"))
}

/// docling's `read_content`: a Keynote 6+ presentation out of its IWA object
/// graph. `archives` are the index's decoded members (the container itself,
/// or its nested `Index.zip`); `container` holds the image data under
/// `data_prefix`.
pub(crate) fn read_content(
    archives: &[Archive],
    container: &mut Package,
    data_prefix: &str,
) -> Result<Presentation, ConversionError> {
    let (objects, order) = index_objects(archives);
    let document = order
        .iter()
        .filter_map(|id| objects.get(id).copied())
        .find(|a| a.ty == KN_DOCUMENT_ARCHIVE)
        .ok_or_else(|| {
            fail(
                "the Keynote document has no KN.DocumentArchive; the container may be corrupt \
                 or password-protected",
            )
        })?;
    let show = reference_field(&document.payload, DOCUMENT_SHOW_FIELD)
        .and_then(|id| objects.get(&id).copied())
        .filter(|a| a.ty == KN_SHOW_ARCHIVE)
        .ok_or_else(|| {
            fail("the Keynote document does not reference a KN.ShowArchive, so it has no slides")
        })?;
    let (width, height) = slide_size(show);
    let mut reader = KeynoteReader {
        inner: {
            // docling reads only a presentation's charts
            // (`KeynoteReader._drawable_blocks`), so this reader asks the
            // shared one for them.
            let mut reader = Reader::new(&objects, &order, container, data_prefix);
            reader.charts = true;
            reader
        },
    };
    Ok(Presentation {
        slides: reader.slides(show),
        width,
        height,
    })
}

/// `slide_size`: the size the slides are laid out at, in points; the
/// pre-widescreen default when it cannot be read.
fn slide_size(show: &Archive) -> (f64, f64) {
    match first_bytes(&show.payload, SHOW_SLIDE_SIZE_FIELD).and_then(read_point) {
        Some((w, h)) if w > 0.0 && h > 0.0 => (w, h),
        _ => (DEFAULT_SLIDE_WIDTH, DEFAULT_SLIDE_HEIGHT),
    }
}

/// `KeynoteReader`: the parts of a presentation that `KN` adds to the shared
/// ones — the walk down the show's slide tree, each slide's placeholders, its
/// presenter notes, and the sticky notes its comments are drawn as.
struct KeynoteReader<'a, 'p> {
    inner: Reader<'a, 'p>,
}

impl<'a> KeynoteReader<'a, '_> {
    /// `slides`: every slide of the show, in presentation order.
    fn slides(&mut self, show: &Archive) -> Vec<Slide> {
        let Some(tree) = first_bytes(&show.payload, SHOW_SLIDE_TREE_FIELD) else {
            return Vec::new();
        };
        reference_list(tree, SLIDE_TREE_NODES_FIELD)
            .into_iter()
            .filter_map(|node| self.slide(node))
            .collect()
    }

    /// `_slide`: the slide one node of the slide tree stands for.
    fn slide(&mut self, node_id: u64) -> Option<Slide> {
        let node = self.inner.typed(node_id, KN_SLIDE_NODE_ARCHIVE)?;
        let slide = reference_field(&node.payload, SLIDE_NODE_SLIDE_FIELD)
            .and_then(|id| self.inner.typed(id, KN_SLIDE_ARCHIVE))?;
        let title = reference_field(&slide.payload, SLIDE_TITLE_FIELD);
        let number = reference_field(&slide.payload, SLIDE_NUMBER_FIELD);

        let placed = self.placed(slide, number);
        let geometries: Vec<Option<Geometry>> = placed.iter().map(|(_, g)| *g).collect();
        let mut blocks = Vec::new();
        let mut comments = Vec::new();
        for position in reading_order(&geometries) {
            let (identifier, geometry) = placed[position];
            let (found, said) = self.blocks(identifier, Some(identifier) == title);
            blocks.extend(found.into_iter().map(|block| Placed { block, geometry }));
            comments.extend(said);
        }
        Some(Slide {
            blocks,
            notes: self.notes(slide),
            comments,
        })
    }

    /// `_placed`: what is on a slide, with where each of it sits — each
    /// drawable's identifier and geometry in stored order, the repeats
    /// between the slide's two accounts of them dropped, the slide number
    /// left out.
    fn placed(&self, slide: &Archive, number: Option<u64>) -> Vec<(u64, Option<Geometry>)> {
        let mut identifiers: Vec<u64> = [SLIDE_TITLE_FIELD, SLIDE_BODY_FIELD]
            .into_iter()
            .filter_map(|field| reference_field(&slide.payload, field))
            .collect();
        identifiers.extend(reference_list(&slide.payload, SLIDE_DRAWABLES_FIELD));

        let mut placed = Vec::new();
        let mut seen: HashSet<u64> = HashSet::new();
        for identifier in identifiers {
            if Some(identifier) == number || !seen.insert(identifier) {
                continue;
            }
            let geometry = self
                .inner
                .object(identifier)
                .and_then(|d| drawable_geometry(&d.payload));
            placed.push((identifier, geometry));
        }
        placed
    }

    /// `_blocks`: whichever kind of drawable `identifier` names on a slide —
    /// the blocks it contributes, and the comments it holds.
    fn blocks(&mut self, identifier: u64, title: bool) -> (Vec<Block>, Vec<Comment>) {
        let Some(drawable) = self.inner.object(identifier) else {
            return (Vec::new(), Vec::new());
        };
        match drawable.ty {
            KN_PLACEHOLDER_ARCHIVE => {
                if !self.inner.emitted.insert(identifier) {
                    return (Vec::new(), Vec::new());
                }
                let blocks = self
                    .placeholder_blocks(drawable)
                    .into_iter()
                    .map(|b| titled(b, title))
                    .collect();
                (blocks, Vec::new())
            }
            KN_COMMENT_ARCHIVE => {
                if !self.inner.emitted.insert(identifier) {
                    return (Vec::new(), Vec::new());
                }
                (Vec::new(), self.comments(drawable))
            }
            // Anything else — a text box, image, table, group, or a chart
            // (`TSCH.ChartDrawableArchive`, docling#4376) — the shared
            // reader reads, charts included since this reader asks for them.
            _ => {
                let blocks = self
                    .inner
                    .drawable_blocks(identifier)
                    .into_iter()
                    .map(|b| titled(b, false))
                    .collect();
                (blocks, Vec::new())
            }
        }
    }

    /// `_placeholder_blocks`: the text of one placeholder, through the shape
    /// info it wraps.
    fn placeholder_blocks(&mut self, placeholder: &'a Archive) -> Vec<Block> {
        let Some(shape) = first_bytes(&placeholder.payload, PLACEHOLDER_SHAPE_FIELD) else {
            return Vec::new();
        };
        let Some(storage) = reference_field(shape, SHAPE_STORAGE_FIELD)
            .and_then(|id| self.inner.typed(id, TSWP_STORAGE_ARCHIVE))
        else {
            return Vec::new();
        };
        self.inner.storage_blocks(storage)
    }

    /// `_comments`: the comment a sticky note on the slide stands for, and its
    /// replies. Keynote's comments annotate the slide, not a stretch of text.
    fn comments(&self, shape: &Archive) -> Vec<Comment> {
        let head = reference_field(&shape.payload, COMMENT_STORAGE_FIELD);
        self.inner
            .thread(head)
            .into_iter()
            .map(|text| Comment {
                text,
                anchor: String::new(),
            })
            .collect()
    }

    /// `_notes`: the presenter notes of one slide.
    fn notes(&self, slide: &Archive) -> Vec<Paragraph> {
        let Some(note) = reference_field(&slide.payload, SLIDE_NOTE_FIELD)
            .and_then(|id| self.inner.typed(id, KN_NOTE_ARCHIVE))
        else {
            return Vec::new();
        };
        self.inner
            .storage_paragraphs(reference_field(&note.payload, NOTE_TEXT_FIELD))
    }
}
