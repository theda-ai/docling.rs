"""Synthesize decks I-K of the #627 list-marker cascade fixtures.

A-H are the reporter's decks (make_repro.py in docling.rs#627). Run with
python-pptx 1.0.2: ``python make_decks.py <out-dir>``.
"""
import sys
from pptx import Presentation
from lxml import etree

A = 'http://schemas.openxmlformats.org/drawingml/2006/main'
P = 'http://schemas.openxmlformats.org/presentationml/2006/main'
ns = {'a': A, 'p': P}
out = sys.argv[1]

def q(tag):
    pre, local = tag.split(':')
    return '{%s}%s' % (ns[pre], local)

def set_marker(lvl_ppr, marker):
    for tag in ('buNone', 'buChar', 'buAutoNum', 'buBlip', 'buFont'):
        for el in lvl_ppr.findall('a:' + tag, ns):
            lvl_ppr.remove(el)
    el = etree.Element(q('a:' + marker[0]))
    for k, v in marker[1].items():
        el.set(k, v)
    # Bullet elements precede a:defRPr in CT_TextParagraphProperties.
    d = lvl_ppr.find('a:defRPr', ns)
    if d is not None:
        d.addprevious(el)
    else:
        lvl_ppr.append(el)

def layout_ph_level(layout, idx, n=1):
    ph = layout.placeholders[idx]
    tx = ph._element.find('.//p:txBody', ns)
    ls = tx.find('a:lstStyle', ns)
    if ls is None:
        ls = etree.SubElement(tx, q('a:lstStyle'))
        tx.insert(1, ls)
    lv = ls.find('a:lvl%dpPr' % n, ns)
    if lv is None:
        lv = etree.SubElement(ls, q('a:lvl%dpPr' % n))
    return lv

def master_style(prs, name):
    return prs.slide_master._element.find('.//p:txStyles/p:' + name, ns)

def fill(tf, lines):
    tf.text = lines[0][0]
    tf.paragraphs[0].level = lines[0][1]
    for text, lvl in lines[1:]:
        p = tf.add_paragraph()
        p.text = text
        p.level = lvl

BODY = [('first', 0), ('second', 0), ('third', 0)]

def deck(name, layout_no, body=BODY, tweak=None, title='T', body_idx=1):
    prs = Presentation()
    if tweak:
        tweak(prs)
    s = prs.slides.add_slide(prs.slide_layouts[layout_no])
    s.shapes.title.text = title
    ph = s.placeholders[body_idx]
    fill(ph.text_frame, body)
    prs.save('%s/%s.pptx' % (out, name))

# I: a slide body placeholder whose idx has no layout counterpart — docling
# only reaches the master through a matching layout placeholder.
prs = Presentation()
s = prs.slides.add_slide(prs.slide_layouts[1])
s.shapes.title.text = 'T'
body = s.placeholders[1]
body._element.find('.//p:nvPr/p:ph', ns).set('idx', '7')
fill(body.text_frame, [('first', 0), ('second', 1)])
prs.save('%s/I_unmatched_layout_idx.pptx' % out)

# J: levels pick lvlNpPr at every step — layout lvl2 buNone, master lvl3 autonum.
def j(prs):
    set_marker(layout_ph_level(prs.slide_layouts[1], 1, 2), ('buNone', {}))
    set_marker(master_style(prs, 'bodyStyle').find('a:lvl3pPr', ns), ('buAutoNum', {'type': 'arabicPeriod'}))
deck('J_levels_layout_then_master', 1, body=[('one', 0), ('two', 1), ('three', 2), ('four', 2), ('five', 0)], tweak=j)

# K: a numbered list starting at its first paragraph's startAt, then a bullet
# joining the numbered group (Markdown numbers it by position).
prs = Presentation()
s = prs.slides.add_slide(prs.slide_layouts[1])
s.shapes.title.text = 'T'
tf = s.placeholders[1].text_frame
fill(tf, [('four', 0), ('five', 0), ('bullet', 0)])
for i, p in enumerate(tf.paragraphs[:2]):
    ppr = p._p.get_or_add_pPr()
    el = etree.SubElement(ppr, q('a:buAutoNum'))
    el.set('type', 'arabicPeriod')
    if i == 0:
        el.set('startAt', '4')
prs.save('%s/K_start_at_and_mixed_group.pptx' % out)
