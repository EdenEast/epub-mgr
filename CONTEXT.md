# EPUB Manager

EPUB Manager helps clean and normalize a personal EPUB library by copying source books into a curated output library with consistent metadata and paths.

## Language

**Source Library**:
The existing collection of EPUB files used as input for a cleanup run. It is treated as read-only.
_Avoid_: Input library, original library

**Output Library**:
The destination root where cleaned EPUB copies are written using normalized paths. It is the completed target library for a cleanup run.
_Avoid_: Destination folder, output folder

**Cleaned EPUB**:
A copied EPUB whose file path follows the output library's normalization rules, based only on metadata already present in the source EPUB.
_Avoid_: Updated original, processed file

**Output Path Template**:
A configurable pattern that turns EPUB metadata into a relative path inside the output library. Optional parts of the pattern are included only when their metadata is present.
_Avoid_: Filename format, destination pattern
