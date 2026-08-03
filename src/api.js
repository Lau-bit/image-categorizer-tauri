'use strict';

const tauri = window.__TAURI__;
const invoke = tauri?.core?.invoke;
const dialog = tauri?.dialog;
const convertFileSrc = tauri?.core?.convertFileSrc;
const event = tauri?.event;
const tauriWindow = tauri?.window;
const webview = tauri?.webview;

if (!invoke || !dialog || !convertFileSrc || !event || !tauriWindow) {
  console.error('Tauri API is not available.');
}

const IMPORT_EXTENSIONS = ['jpg', 'jpeg', 'png', 'gif', 'bmp', 'webp', 'tiff', 'tif', 'heic', 'heif'];

window.categorizerAPI = {
  showWindow: () => tauriWindow?.getCurrentWindow?.()?.show?.(),

  getSettings: () => invoke('get_app_settings'),
  setTileSize: tileSize => invoke('set_tile_size', { tileSize }),
  setDarkMode: darkMode => invoke('set_dark_mode', { darkMode }),

  // Automatic daily refresh (headless, Task-Scheduler-driven)
  getAutoRefreshSettings: () => invoke('get_auto_refresh_settings'),
  setAutoRefreshSettings: settings => invoke('set_auto_refresh_settings', settings),

  scanLibrary: root => invoke('scan_library', { root }),
  setSourcePattern: (root, preset, regex) => invoke('set_source_pattern', { root, preset, regex }),
  addManualSourceFolder: (root, folderPath) => invoke('add_manual_source_folder', { root, folderPath }),
  removeManualSourceFolder: (root, folderName) => invoke('remove_manual_source_folder', { root, folderName }),

  // Text (OCR) analysis
  analyzeText: (root, force) => invoke('analyze_text', { root, force }),
  cancelTextAnalysis: () => invoke('cancel_text_analysis'),
  setTextThresholds: (root, wordThreshold, areaThreshold) =>
    invoke('set_text_thresholds', { root, wordThreshold, areaThreshold }),
  setFolderAnalysisIncluded: (root, folderName, included) =>
    invoke('set_folder_analysis_included', { root, folderName, included }),
  setCategoryAnalysisIncluded: (root, categoryName, included) =>
    invoke('set_category_analysis_included', { root, categoryName, included }),
  onTextAnalysisProgress: callback => event.listen('text-analysis-progress', message => callback(message.payload)),
  onTextAnalysisFinished: callback => event.listen('text-analysis-finished', message => callback(message.payload)),

  // OCR text extraction (saves recognized text to a sidecar folder)
  extractText: (root, force) => invoke('extract_text', { root, force }),
  cancelTextExtraction: () => invoke('cancel_text_extraction'),
  onTextExtractionProgress: callback => event.listen('text-extraction-progress', message => callback(message.payload)),
  onTextExtractionFinished: callback => event.listen('text-extraction-finished', message => callback(message.payload)),

  // NSFW (explicit content) analysis
  analyzeNsfw: (root, force) => invoke('analyze_nsfw', { root, force }),
  cancelNsfwAnalysis: () => invoke('cancel_nsfw_analysis'),
  setNsfwThreshold: (root, threshold) => invoke('set_nsfw_threshold', { root, threshold }),
  getNsfwModelInfo: () => invoke('get_nsfw_model_info'),
  downloadNsfwModel: () => invoke('download_nsfw_model'),
  onNsfwAnalysisProgress: callback => event.listen('nsfw-analysis-progress', message => callback(message.payload)),
  onNsfwAnalysisFinished: callback => event.listen('nsfw-analysis-finished', message => callback(message.payload)),

  // Video de-duplication: OCR the title bar, group frames by video, sample N. Produces a standalone
  // chunk plan file the vision pass then reads.
  buildChunkPlan: (root, force) => invoke('build_chunk_plan', { root, force }),
  cancelChunkScan: () => invoke('cancel_chunk_scan'),
  getChunkPlan: root => invoke('get_chunk_plan', { root }),
  regenerateChunkPlan: root => invoke('regenerate_chunk_plan', { root }),
  discardChunkPlan: root => invoke('discard_chunk_plan', { root }),
  onChunkScanProgress: callback => event.listen('chunk-scan-progress', message => callback(message.payload)),
  onChunkScanFinished: callback => event.listen('chunk-scan-finished', message => callback(message.payload)),

  // Vision descriptions (images → words) via a local OpenAI-compatible model.
  analyzeVision: (root, force) => invoke('analyze_vision', { root, force }),
  cancelVisionAnalysis: () => invoke('cancel_vision_analysis'),
  getVisionSettings: () => invoke('get_vision_settings'),
  setVisionSettings: (endpoint, model, apiKey) => invoke('set_vision_settings', { endpoint, model, apiKey }),
  listVisionModels: () => invoke('list_vision_models'),
  loadVisionModel: model => invoke('load_vision_model', { model }),
  onVisionAnalysisProgress: callback => event.listen('vision-analysis-progress', message => callback(message.payload)),
  onVisionAnalysisFinished: callback => event.listen('vision-analysis-finished', message => callback(message.payload)),

  // Geo layer: countries derived from the vision descriptions, plus country sets built from them.
  // Purely additive — these read/write their own sidecars and never touch image categories.
  deriveGeo: root => invoke('derive_geo', { root }),
  // Scene-kind pass: text-only over the descriptions, so country sets can drop interiors,
  // talking heads and screenshots that carry a real country but show no place.
  classifyKinds: (root, force) => invoke('classify_kinds', { root, force }),
  cancelKindClassification: () => invoke('cancel_kind_classification'),
  getKindSummary: root => invoke('get_kind_summary', { root }),
  // Re-runs only the inheritance step (which unlooked-at frames may take their video's kind) from
  // the labels already on disk. No model, instant — the way a changed propagation rule reaches a
  // library without re-classifying it.
  repropagateKinds: root => invoke('repropagate_kinds', { root }),
  onKindProgress: callback => event.listen('kind-classification-progress', message => callback(message.payload)),
  onKindFinished: callback => event.listen('kind-classification-finished', message => callback(message.payload)),
  getGeoSummary: root => invoke('get_geo_summary', { root }),
  getGeoCoverage: root => invoke('get_geo_coverage', { root }),
  buildGeoSets: (root, targetSize) => invoke('build_geo_sets', { root, targetSize }),
  getGeoSets: root => invoke('get_geo_sets', { root }),
  // Whether what the panel is showing has been overtaken by something written since — including by
  // super-image-viewer, which writes the exclusion list. Cheap (never parses the records file), so
  // it can be re-checked whenever the window comes back rather than only on a full reload.
  getGeoStatus: root => invoke('get_geo_status', { root }),
  getGeoSetImages: (root, setId) => invoke('get_geo_set_images', { root, setId }),
  openGeoGazetteer: root => invoke('open_geo_gazetteer', { root }),
  // The worklist's decision table. `setGeoOverride` writes the gazetteer and nothing else — the
  // decision reaches the records on the next derive, which the worklist says out loud.
  // action: 'place' (with a country, or "A, B" for a route) | 'reject' | 'clear'.
  getGeoOverrides: root => invoke('get_geo_overrides', { root }),
  // The frames a worklist string actually came from, for deciding it by eye. Returns hashes +
  // descriptions; the caller resolves the hashes against the library it already holds.
  getGeoLocationImages: (root, location, limit) =>
    invoke('get_geo_location_images', { root, location, limit }),
  setGeoOverride: (root, location, action, country) =>
    invoke('set_geo_override', { root, location, action, country: country ?? null }),
  // Post-build review: what is wrong with the sets that already exist, and the fixes for it.
  // `applyGeoReview` only writes the exclusion list and the gazetteer — re-deriving and rebuilding
  // stay separate calls so the user sees them happen.
  reviewGeoSets: root => invoke('review_geo_sets', { root }),
  applyGeoReview: (root, fixes) => invoke('apply_geo_review', { root, fixes }),

  // Category management
  createCategory: (root, name) => invoke('create_category', { root, name }),
  renameCategory: (root, oldName, newName) => invoke('rename_category', { root, oldName, newName }),
  deleteCategory: (root, name) => invoke('delete_category', { root, name }),

  assignCategory: (root, hash, category) => invoke('assign_category', { root, hash, category }),
  // relativePath says which FILE to move: duplicates share one hash, so the hash alone is ambiguous.
  moveImage: (root, hash, relativePath, targetFolder) =>
    invoke('move_image', { root, hash, relativePath, targetFolder }),

  // Manual import: copy images (or whole folders of them) from anywhere into a library subfolder.
  importImages: (root, targetFolder, paths) => invoke('import_images', { root, targetFolder, paths }),

  chooseImagesToImport: async () => {
    const selection = await dialog.open({
      title: 'Choose Images to Import',
      multiple: true,
      filters: [{ name: 'Images', extensions: IMPORT_EXTENSIONS }],
    });
    if (!selection) return null;
    return Array.isArray(selection) ? selection : [selection];
  },

  chooseFolderToImport: async () => {
    const folderPath = await dialog.open({
      title: 'Choose a Folder of Images to Import',
      directory: true,
      multiple: false,
    });
    if (!folderPath) return null;
    return [folderPath];
  },

  // Fires while files are dragged over the window and when they land. `dragDropEnabled` defaults to
  // true on the Tauri window, which suppresses the webview's own HTML5 drop events — so this is the
  // only way to see an OS drag, and it's also the only way to learn the real on-disk paths.
  // Tauri emits 'enter' | 'over' | 'drop' | 'leave'. 'enter' fires first and must show the overlay
  // too — treating it as a cancel made the overlay blink off before the first 'over' restored it.
  onFileDrop: callback =>
    webview?.getCurrentWebview?.()?.onDragDrop?.(dropEvent => {
      const { type, paths } = dropEvent.payload;
      if (type === 'enter' || type === 'over') callback({ state: 'over' });
      else if (type === 'drop') callback({ state: 'drop', paths: paths || [] });
      else callback({ state: 'cancel' });
    }),

  openImage: filePath => invoke('open_image', { filePath }),
  revealImage: filePath => invoke('reveal_image', { filePath }),
  openRootFolder: root => invoke('open_root_folder', { root }),

  getFileUrl: filePath => convertFileSrc(filePath),

  chooseRootFolder: async currentPath => {
    const folderPath = await dialog.open({
      title: 'Choose Image Library Root Folder',
      defaultPath: currentPath || undefined,
      directory: true,
      multiple: false,
    });
    if (!folderPath) return null;
    return invoke('choose_root_folder', { folderPath });
  },

  selectRootFolder: rootPath => invoke('select_root_folder', { root: rootPath }),

  chooseManualSourceFolder: async root => {
    const folderPath = await dialog.open({
      title: 'Choose a Source Subfolder',
      defaultPath: root || undefined,
      directory: true,
      multiple: false,
    });
    if (!folderPath) return null;
    return invoke('add_manual_source_folder', { root, folderPath });
  },
};
