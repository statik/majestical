Research complete. Here is the structured report.

---

# NeoFinder (cdfinder.de) — Research Report

## 1. Core purpose and target user

NeoFinder is a macOS disk-cataloging / digital asset manager by Norbert M. Doerner (West-Forest-Systems), the continuation of **CDFinder**, on the market since 1996 — roughly 30 years of continuous development ([versions.html](https://www.cdfinder.de/versions.html)). It indexes volumes into portable **catalog files** so that file names, folder structure, metadata, and thumbnails remain searchable when the drive is disconnected ([guide 1/info](https://www.cdfinder.de/guide/1/info.html)).

Target users: photographers, videographers, agencies, and archives whose storage is fragmented across many external drives, NAS, LTO tapes, and optical media. The vendor's own PR claims 150,000+ users in 113 countries and names NYT, BBC, Disney, Abbey Road, NASA, and Jung von Matt as customers ([PR 9.2.1 PDF](https://www.wfs-apps.de/PR/PR.NeoFinder.9.2.1.en.pdf)) — self-reported, uncorroborated. A Windows twin, **abeMeda** (formerly CDWinder), shares the catalog format; an **iOS app** puts catalogs on iPhone/iPad with a light table ([cdfinder.de](https://www.cdfinder.de/)).

Current version: **9.2.1**, released March 30, 2026. Requires macOS 10.15+, certified for macOS 26 "Tahoe", Intel and Apple Silicon.

## 2. Key features

**Offline cataloging** of SSD/HDD (HFS+, APFS, NTFS, ExFAT, FAT32), AFP/SMB/NAS/FTP, Dropbox, Backblaze B2, Box, Amazon S3, Blu-ray, LTO, SD/CF/microSD, Audio-CDs. Batch Catalog auto-catalogs and ejects each inserted disc; AutoUpdater (Business license) runs scheduled updates; parallel cataloging of multiple volumes is supported ([guide 3](https://cdfinder.de/guide/3/cataloging.html)).

**Thumbnails/previews stored in the catalog** for photos, video, and audio cover art — this is what makes offline visual browsing work, and it dominates catalog size.

**Metadata**: EXIF, IPTC, XMP, ID3/MP3, video metadata (via bundled EXIFTool/FFmpeg), archive contents (ZIP, TAR, RAR, StuffIt, disk images), plus 5 user-defined custom fields and **MD5 checksums per file** ("FileCheck"). Native support for R3D (RED), BRAW, Affinity Photo/Designer, Phase One EIP, and FCPX keywords. An integrated **XMP editor** writes ratings, keywords, captions back into the original files — so metadata survives the app.

**GPS/map**: catalogs coordinates *plus* altitude, view direction, tilt, distance. Inspector map with satellite view; **GeoFinder** searches all geotagged items within the visible map rectangle; pins can be dragged to correct locations; geotags can be written to photos and .mov/.mp4; KMZ export; reverse geocoding and a Wikipedia inspector ([guide 20](https://cdfinder.de/guide/20/neofinder_geotagging.html)).

**Duplicates**: match by name (optionally ignoring extension), MD5, or ISRC, constrained by size/date/file kind ([guide 5.3](https://www.cdfinder.de/guide/5/5.3/find_duplicates.html)).

**Batch rename** with a variable-substitution scheme and collision warnings ([guide 8.14](https://cdfinder.de/guide/8/8.14/multi_rename.html)).

**macOS integration**: Finder Services and context menus, Quick Look, AppleScript, a menu-bar search app, Drag&Drop into InDesign/Quark/Pages/Office, FileMaker Pro integration, Roxio Toast auto-cataloging.

**Team sharing**: Business licenses allow the database folder to live on a server/NAS (including Linux/Windows hosts), with a "keep looking for changes" sync so multiple Macs (and abeMeda on Windows) see each other's catalog additions. Also READER mode for search-only users, shared Smart Folders, controlled vocabulary, Web Gallery HTML export, XML export.

**AI features — yes, and Apple Vision specifically.** NeoFinder's **AutoTags** system is pluggable: a **macOS engine using Apple's Vision framework** (bundled since 8.3), **MobileNetV2**, and **Inception V3**, all running **entirely locally, no server transfer** ([guide 30.3](https://cdfinder.de/guide/30/30.3/download_engines.html)). There is a documented Engine API for writing your own in Xcode. "Analyse thumbnails" batch-runs three operations: **face detection**, **CoreML AutoTag generation** (stored in a dedicated catalog field for fast search), and **OCR**, which drops recognized text into the catalog's Contents field so images become text-searchable ([guide 30.5](https://www.cdfinder.de/guide/30/30.5/image_analysis.html)). The AutoTags Inspector shows candidate tags with match probabilities and one-click promotion to XMP keywords; tags are English-only. Since 9.1 it also ingests `.vtag` sidecars from the third-party **VisionaryAI** tool, which adds video audio transcription ([guide 30.6](https://cdfinder.de/guide/30/30.6/nf_visionary_AI.html)).

## 3. Catalog data model

**One file per cataloged volume or folder**, suffix `.neofinder7` (the "extended" format, mandatory since v8), all sitting in a single **NeoFinder Database folder** — by default `~/Library/Application Support/NeoFinder/NeoFinder Database`, but relocatable to any folder including a shared/cloud one ([guide 4.5](https://www.cdfinder.de/guide/4/4.5/neofinder_backup.html), [copperhound.com](https://www.copperhound.com/blog/managing-hard-drives)).

This is the model's main strength: backup is "copy the folder," files are named after the volume they represent, and only changed catalogs need re-backup. No monolithic database.

**Size on disk** is driven almost entirely by thumbnails. Copperhound measured roughly **2 MB per 1,000 files at 256px thumbnails**; their 128 catalogs totaled ~4 GB, with a single 400,000-file drive producing a **982 MB** catalog. The vendor's own guide shows 21 catalogs consuming **10 GB** because of thumbnails ([guide 11](https://cdfinder.de/guide/11/performance.html)). Scale ceilings are high — the same page cites a customer with **57,000 catalogs**, and forum posts reference catalogs exceeding **15 million files**. Grouping catalogs into subfolders is required at high counts; one customer's 7,000 flat catalogs caused stalls until foldered.

Portability also runs inward: importers exist for ~12–25 legacy formats including iView MediaPro, Expression Media, Canto Cumulus, Extensis Portfolio, Delicious Library, DiskTracker, DiskCatalogMaker, and Advanced Disk Catalog — making it one of the few live migration paths off dead DAMs.

## 4. Search

Three tiers. **QuickFind** is a global instant search (Cmd-Shift-F, or a menu-bar app), scopable to selected catalogs or to photos/video/audio only. The **Find Editor** is the real engine: a top-level **AND/OR selector** ("All of the following are true" / "Any of the following"), catalog scoping by selection or color label, and **up to 16 criteria**, including relative dates (days/weeks/months/years), EXIF capture date, and even **EXIF season** (Northern-Hemisphere Winter/Spring/Summer/Autumn) ([guide 5.2](https://www.cdfinder.de/guide/5/5.2/findeditor.html)). Note the ceiling of 16 criteria and the single flat AND-or-OR — there is no nested boolean grouping.

**Smart Folders** are saved searches, re-evaluated live on selection, groupable and color-labelable. They are stored as plain `.query` XML files in `~/Library/Application Support/NeoFinder/SmartFolders/`, so admins can distribute a standard set to a team ([guide 6](https://www.cdfinder.de/guide/6/smartfolders.html)).

Also: **Find Similar Photos** (by pixel motif with a similarity slider, by dominant/top-10 color, or by shared AutoTags), Find Faces, Find Duplicates, and **Search URLs** for launching queries from outside the app.

**Spotlight integration is one-directional.** NeoFinder can query the Spotlight index alongside its own catalogs in a single search, mapping its EXIF/IPTC parameters onto Spotlight's. But there is **no Spotlight plugin for catalog files** — you cannot search NeoFinder catalogs from the Finder. The developer explains this as architectural: Spotlight indexes metadata per discrete file, while one catalog file may hold hundreds of thousands of records ([guide 5.5](https://www.cdfinder.de/guide/5/5.5/neofinder_spotlight.html)).

## 5. Pricing / licensing

Perpetual, **no subscription** ([store.html](https://www.cdfinder.de/store.html)):

| License | Price | Notes |
|---|---|---|
| Demo | Free, 30 days | Max 10 catalogs; some features disabled |
| Private | **$39.99** | One user, installable on up to 3 personal Macs. **Excludes** server catalog sharing, AutoUpdater, and database sync |
| Business 2-user | **$149** | Adds network sharing, sync, AutoUpdater, READER mode, Web Gallery/XML export |
| Business 3/5/10/20/50/Site | Quote | Any package size available |
| Upgrade (Private) | **$25.99** | From CDFinder, v6, v7, or v8 |

v9 is a **paid** upgrade from all prior versions, though v8 licenses bought after Jan 1, 2025 get v9 free ([upgrade8.html](https://www.cdfinder.de/upgrade8.html)). Cross-grades from competing products exist. One reviewer flags that pricing isn't visible from the main marketing site ([mycatisfat.de](https://mycatisfat.de/en/tools/neofinder)).

## 6. What users praise / complain about

**Praise** centers on value and durability. "Worth far more than the measly $40 the author asks" ([dpreview](https://www.dpreview.com/forums/threads/neofinder-turns-out-to-be-a-great-dam-solution.4188191/)); "inexpensive with no subscription... the app just turned 30, so, Lindy Effect" ([baty.net](https://baty.net/posts/2025/12/neofinder-as-photo-catalog-on-macos/)). The developer's responsiveness is repeatedly singled out — "one of the most responsive developers I have encountered" ([Call-A.P.P.L.E.](https://www.callapple.org/modern-apple-computing/neofinder-8/)). A recurring structural praise: **no lock-in**, since metadata is written into the files themselves — "should they go out of business in a year, I've lost no functionality." Reviewers also note enterprise-grade breadth at hobbyist pricing ([visualsproducer](https://visualsproducer.wordpress.com/2020/04/15/neofinder-7-is-a-full-scale-macos-digital-asset-management-app/)).

**Complaints**, roughly in order of severity:

- **Cataloging speed with thumbnails enabled is the big one.** A MacRumors head-to-head had DiskCatalogMaker 80% complete in 10 minutes while NeoFinder was under 10%, and **still under 20% after five hours**, forcing the tester to cancel ([forums.macrumors.com](https://forums.macrumors.com/threads/diskcatalogmaker-vs-neofinder-vs-some_other.2238927/)). With thumbnails off, the same user indexed a 30 TB NAS in about an hour. The vendor maintains a whole "Performance Tuning" chapter advising smaller per-folder catalogs and disabling antivirus — an implicit acknowledgment.
- **No live sync.** Catalogs must be manually updated to reflect drive changes; "I still don't like the need to force an update of a catalog to show added or changed files." The community workaround is splitting libraries into per-year catalogs so only one needs updating.
- **Not a photo-editing DAM.** No versions/stacks; you can't move files around freely (a holdover from its CD-cataloging lineage); "very meh as a replacement for Aperture, Lr or Photos."
- **Thumbnail scroll lag** in large galleries versus Media Pro.
- **UI age** — v9's release notes lead with "improved user interface with crystal clear icons, nicely legible text, and no distracting transparencies," which reads as a response to complaints about a dated, utilitarian look.
- **Mac-only** (Windows requires buying separate abeMeda), and it **rewards metadata discipline** — reviewers warn it's "not for someone simply looking for a replacement for Apple Photos without investing time in metadata" ([AppAddict](https://appaddict.app/post/neofinder-the-mac-app-that-makes-offline-drives-searchable)).
