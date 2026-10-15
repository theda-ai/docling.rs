"""Picture description with any LangChain chat model — the port of
``langchain_docling.picture_description``.

upstream registers a docling plugin; here the options plug into
:class:`docling_rs.PdfPipelineOptions` directly (``allow_external_plugins`` is
accepted but not needed)::

    from langchain_openai import ChatOpenAI
    from docling_rs import DocumentConverter, InputFormat, PdfFormatOption, PdfPipelineOptions
    from docling_rs.langchain.picture_description import PictureDescriptionLangChainOptions

    llm = ChatOpenAI(model="granite-vision-3.2-2b", base_url="http://localhost:1234/v1", api_key="none")
    opts = PdfPipelineOptions(do_picture_description=True)
    opts.picture_description_options = PictureDescriptionLangChainOptions(
        llm=llm, prompt="Describe the image in three sentences.", provenance="granite-vision"
    )
    doc = DocumentConverter(
        format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=opts)}
    ).convert("paper.pdf").document
    for pic in doc.pictures:
        if pic.meta and pic.meta.description:
            print(pic.meta.description.created_by, pic.meta.description.text)

The request per picture is upstream's: one user message with the prompt and
the picture as a base64 PNG ``image_url``, all of a batch sent through
``llm.batch(...)``.
"""

from __future__ import annotations

import base64
import io
from dataclasses import dataclass
from typing import Any, ClassVar, Iterable, List, Optional

from ..picture_description import PictureDescriptionBaseOptions

__all__ = ["PictureDescriptionLangChainOptions"]


@dataclass
class PictureDescriptionLangChainOptions(PictureDescriptionBaseOptions):
    """Options for describing pictures with a LangChain chat model.

    :param llm: a LangChain ``BaseChatModel`` that accepts image input
        (required).
    :param prompt: the instruction sent with every picture.
    :param provenance: suffix of ``created_by`` (``"langchain-<provenance>"``),
        e.g. the model id.
    """

    kind: ClassVar[str] = "langchain"

    llm: Any = None
    prompt: str = "Describe this document picture in a few sentences."
    provenance: Optional[str] = None

    def __post_init__(self) -> None:
        if self.llm is None:
            raise ValueError("PictureDescriptionLangChainOptions(llm=...) is required")

    @property
    def provenance_label(self) -> str:
        return f"langchain-{self.provenance}" if self.provenance else "langchain"

    def _annotate_images(self, images: List[Any]) -> Iterable[str]:
        batch_messages = []
        for image in images:
            buffered = io.BytesIO()
            image.save(buffered, format="PNG")
            image_data = base64.b64encode(buffered.getvalue()).decode("utf-8")
            batch_messages.append(
                [
                    {
                        "role": "user",
                        "content": [
                            {"type": "text", "text": self.prompt},
                            {
                                "type": "image_url",
                                "image_url": {"url": f"data:image/png;base64,{image_data}"},
                            },
                        ],
                    }
                ]
            )
        for resp in self.llm.batch(batch_messages):
            text = getattr(resp, "text", resp)
            # langchain-core 1.x: a str property (callable only for
            # back-compat, deprecated); 0.3: a method.
            yield str(text) if isinstance(text, str) or not callable(text) else text()
