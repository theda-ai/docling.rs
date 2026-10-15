"""Picture description — docling's ``do_picture_description`` enrichment as a
post-conversion pass over the ``DoclingDocument``.

docling runs picture description as a pipeline stage on page-image crops;
docling.rs's engine already embeds every picture's crop in the document
(``picture.image``, a PNG data URI), so the stage runs here, in Python, on the
converted document — which is also what lets any Python-side model
(e.g. a LangChain chat model, :mod:`docling_rs.langchain.picture_description`)
plug in without touching the Rust engine.

The selection and output follow docling's ``PictureDescriptionBaseModel``
(docling 2.132): pictures smaller than ``picture_area_threshold`` of their
page are skipped (pictures without page provenance — DOCX, HTML, … — are
always described), the optional classification allow/deny lists filter on
``meta.classification`` (``do_picture_classification``), images go to the
model in batches of ``batch_size``, and each description lands in
``picture.meta.description`` (``DescriptionMetaField``, ``created_by`` = the
model's provenance) plus the deprecated ``picture.annotations`` entry docling
still writes by default.

Enable it docling-style::

    opts = PdfPipelineOptions(do_picture_description=True,
                              picture_description_options=SomeOptions(...))
    DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=opts)})
"""

from __future__ import annotations

import warnings
from dataclasses import dataclass
from typing import Any, ClassVar, Iterable, List, Optional

__all__ = ["PictureDescriptionBaseOptions", "describe_pictures"]


@dataclass
class PictureDescriptionBaseOptions:
    """docling's ``PictureDescriptionBaseOptions``: shared selection/batching
    knobs. Subclasses implement :meth:`_annotate_images` (one description
    string per image) and set :attr:`provenance_label`.

    ``scale`` is accepted for docling compatibility; the engine's embedded
    picture crop is described as is."""

    kind: ClassVar[str] = "base"

    batch_size: int = 8
    scale: float = 2.0
    picture_area_threshold: float = 0.05
    classification_allow: Optional[List[Any]] = None
    classification_deny: Optional[List[Any]] = None
    classification_min_confidence: float = 0.0

    @property
    def provenance_label(self) -> str:
        """``created_by`` of the descriptions."""
        return self.kind

    def _annotate_images(self, images: List[Any]) -> Iterable[str]:
        """One description per PIL image, in order."""
        raise NotImplementedError


def _label_value(label: Any) -> str:
    return getattr(label, "value", None) or str(label)


def _meets_confidence(confidence: Optional[float], min_confidence: float) -> bool:
    return min_confidence <= 0 or (confidence is not None and confidence >= min_confidence)


def _passes_classification(meta: Any, opts: PictureDescriptionBaseOptions) -> bool:
    """docling's ``_passes_classification``, verbatim in behavior."""
    allow, deny = opts.classification_allow, opts.classification_deny
    if not allow and not deny:
        return True
    predicted = None
    if meta is not None and getattr(meta, "classification", None):
        predicted = meta.classification.predictions
    if not predicted:
        return allow is None
    min_conf = opts.classification_min_confidence
    if deny:
        deny_set = {_label_value(label) for label in deny}
        for entry in predicted:
            if _meets_confidence(entry.confidence, min_conf) and entry.class_name in deny_set:
                return False
    if allow:
        allow_set = {_label_value(label) for label in allow}
        return any(
            _meets_confidence(entry.confidence, min_conf) and entry.class_name in allow_set
            for entry in predicted
        )
    return True


def _large_enough(doc: Any, item: Any, threshold: float) -> bool:
    if not item.prov:
        return True
    prov = item.prov[0]  # PictureItems have at most a single provenance
    page = doc.pages.get(prov.page_no)
    if page is None:
        return True
    page_area = page.size.width * page.size.height
    if page_area <= 0:
        return True
    return prov.bbox.area() / page_area >= threshold


def describe_pictures(doc: Any, options: PictureDescriptionBaseOptions) -> int:
    """Describe the document's pictures in place with ``options``' model;
    returns how many were described. Pictures without an embedded image are
    skipped (nothing to show the model)."""
    from docling_core.types.doc.document import PictureDescriptionData

    try:
        from docling_core.types.doc import DescriptionMetaField, PictureMeta
    except ImportError:  # docling-core before the `meta` model: annotations only
        DescriptionMetaField = PictureMeta = None

    if options.batch_size < 1:
        raise ValueError("Picture description batch_size must be >= 1")
    selected = []
    for item in doc.pictures:
        if item.image is None:
            continue
        if not _large_enough(doc, item, options.picture_area_threshold):
            continue
        if not _passes_classification(item.meta, options):
            continue
        selected.append(item)

    provenance = options.provenance_label
    done = 0
    for start in range(0, len(selected), options.batch_size):
        batch = selected[start : start + options.batch_size]
        images = [item.image.pil_image.convert("RGB") for item in batch]
        for item, text in zip(batch, options._annotate_images(images)):
            with warnings.catch_warnings():
                # docling still fills the deprecated field by default.
                warnings.simplefilter("ignore", DeprecationWarning)
                item.annotations.append(PictureDescriptionData(text=text, provenance=provenance))
            if PictureMeta is not None:
                if item.meta is None:
                    item.meta = PictureMeta()
                item.meta.description = DescriptionMetaField(text=text, created_by=provenance)
            done += 1
    return done
