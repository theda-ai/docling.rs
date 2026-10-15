"""LangChain integration for docling.rs — the port of docling-project's
``langchain-docling`` (``pip install 'docling-rs[langchain]'``).

Swap the import, keep the code::

    # was: from langchain_docling import DoclingLoader
    from docling_rs.langchain import DoclingLoader

* :class:`DoclingLoader` (``docling_rs.langchain.loader``) — LangChain document
  loader: one ``Document`` per input (``ExportType.MARKDOWN``) or per chunk
  (``ExportType.DOC_CHUNKS``, the default), with docling's chunk metadata.
* :class:`PictureDescriptionLangChainOptions`
  (``docling_rs.langchain.picture_description``) — describe document pictures
  with any LangChain chat model during conversion.
"""

from .loader import (
    BaseMetaExtractor,
    ConversionBackend,
    DoclingLoader,
    ExportType,
    MetaExtractor,
)
from .picture_description import PictureDescriptionLangChainOptions

__all__ = [
    "BaseMetaExtractor",
    "ConversionBackend",
    "DoclingLoader",
    "ExportType",
    "MetaExtractor",
    "PictureDescriptionLangChainOptions",
]
