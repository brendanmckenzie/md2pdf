use lopdf::Object as LObj;

use crate::renderer::PG_H;

/// 1mm in PDF user-space points.
const MM_TO_PT: f32 = 2.8346;

/// Position of a heading, collected during rendering for the PDF outline.
pub struct HeadingPos {
    pub title: String,
    pub level: u8,
    /// 0-based page index.
    pub page_idx: usize,
    /// Y from the top of the page in mm, pointing at the top of the heading text.
    pub y_top_mm: f32,
}

/// Build a hierarchical PDF outline (bookmark tree) from the collected headings
/// and inject it into the already-serialised PDF bytes via lopdf.
///
/// The tree is built with a parent-stack algorithm: as we process headings in
/// order, we pop the stack until the top has a strictly lower level than the
/// current heading, making it the current heading's parent.
pub fn inject_outline(pdf_bytes: Vec<u8>, headings: &[HeadingPos]) -> Vec<u8> {
    if headings.is_empty() {
        return pdf_bytes;
    }

    let mut doc = lopdf::Document::load_mem(&pdf_bytes)
        .expect("lopdf failed to parse printpdf output");

    // Force traditional xref table output. printpdf emits a cross-reference
    // stream which lopdf can round-trip, but some PDF readers (and notably
    // pdfimages) get confused by the resulting xref entries for image
    // XObjects. Switching to the classic format keeps the byte-offset xref
    // accurate after lopdf re-serializes.
    doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;

    // lopdf page map: 1-based page number → ObjectId
    let pages = doc.get_pages();

    // Pre-allocate one ObjectId per heading, plus one for the root.
    let root_id = doc.new_object_id();
    let item_ids: Vec<lopdf::ObjectId> =
        (0..headings.len()).map(|_| doc.new_object_id()).collect();

    // ── Build tree structure ──────────────────────────────────────────────────
    //
    // parent[i]   = index of parent item, or None (root-level)
    // children[i] = ordered list of child indices
    // prev[i]     = previous sibling index
    // next[i]     = next sibling index

    let n = headings.len();
    let mut parent: Vec<Option<usize>> = vec![None; n];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut prev: Vec<Option<usize>> = vec![None; n];
    let mut next: Vec<Option<usize>> = vec![None; n];

    // Stack holds indices of "open" ancestor items (ordered lowest→highest level).
    let mut stack: Vec<usize> = Vec::new();

    for i in 0..n {
        let level = headings[i].level;
        // Pop ancestors whose level is >= current level — they are closed by this heading.
        while let Some(&top) = stack.last() {
            if headings[top].level >= level {
                stack.pop();
            } else {
                break;
            }
        }
        let par = stack.last().copied();
        parent[i] = par;

        // Link as sibling of the last child of our parent (if any).
        let siblings = match par {
            Some(p) => &mut children[p],
            None => {
                stack.push(i);
                continue;
            }
        };
        if let Some(&prev_sib) = siblings.last() {
            prev[i] = Some(prev_sib);
            next[prev_sib] = Some(i);
        }
        siblings.push(i);
        stack.push(i);
    }

    // Re-run to collect root-level items properly (items with parent = None).
    let root_children: Vec<usize> = (0..n).filter(|&i| parent[i].is_none()).collect();
    // Re-link root children's prev/next (the loop above skipped them).
    for (pos, &i) in root_children.iter().enumerate() {
        if pos > 0 {
            let p = root_children[pos - 1];
            prev[i] = Some(p);
            next[p] = Some(i);
        }
    }

    // ── Create lopdf objects ──────────────────────────────────────────────────

    for (i, heading) in headings.iter().enumerate() {
        // PDF page numbers are 1-based.
        let page_num = (heading.page_idx + 1) as u32;
        let page_ref = pages.get(&page_num).copied().unwrap_or_else(|| {
            // Fall back to the last page if somehow out of range.
            *pages.iter().next_back().unwrap().1
        });

        // Destination: [page_ref /XYZ 0 y 0]
        // Y is bottom-relative in PDF points.
        let y_pt = (PG_H - heading.y_top_mm) * MM_TO_PT;
        let dest = LObj::Array(vec![
            LObj::Reference(page_ref),
            LObj::Name(b"XYZ".to_vec()),
            LObj::Integer(0),
            LObj::Real(y_pt),
            LObj::Integer(0),
        ]);

        let par_ref = match parent[i] {
            Some(p) => LObj::Reference(item_ids[p]),
            None => LObj::Reference(root_id),
        };

        let mut dict = lopdf::Dictionary::new();
        dict.set("Title", LObj::string_literal(heading.title.as_bytes().to_vec()));
        dict.set("Parent", par_ref);
        dict.set("Dest", dest);
        // Positive count = children are open/visible in the panel.
        dict.set("Count", LObj::Integer(children[i].len() as i64));

        if let Some(p) = prev[i] {
            dict.set("Prev", LObj::Reference(item_ids[p]));
        }
        if let Some(nx) = next[i] {
            dict.set("Next", LObj::Reference(item_ids[nx]));
        }
        if let Some(&first) = children[i].first() {
            dict.set("First", LObj::Reference(item_ids[first]));
        }
        if let Some(&last) = children[i].last() {
            dict.set("Last", LObj::Reference(item_ids[last]));
        }

        doc.objects.insert(item_ids[i], LObj::Dictionary(dict));
    }

    // ── Root outlines dictionary ──────────────────────────────────────────────

    let mut root_dict = lopdf::Dictionary::new();
    root_dict.set("Type", LObj::Name(b"Outlines".to_vec()));
    root_dict.set("Count", LObj::Integer(root_children.len() as i64));
    if let Some(&first) = root_children.first() {
        root_dict.set("First", LObj::Reference(item_ids[first]));
    }
    if let Some(&last) = root_children.last() {
        root_dict.set("Last", LObj::Reference(item_ids[last]));
    }
    doc.objects.insert(root_id, LObj::Dictionary(root_dict));

    // ── Patch the document catalog ────────────────────────────────────────────

    let catalog_id = doc
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|o| o.as_reference().ok())
        .expect("PDF has no /Root catalog");

    if let Some(LObj::Dictionary(catalog)) = doc.objects.get_mut(&catalog_id) {
        catalog.set("Outlines", LObj::Reference(root_id));
        // Ask viewers to show the bookmarks panel on open.
        catalog.set("PageMode", LObj::Name(b"UseOutlines".to_vec()));
    }

    // ── Serialise ────────────────────────────────────────────────────────────

    let mut out = Vec::new();
    doc.save_to(&mut out).expect("lopdf serialization failed");
    out
}
