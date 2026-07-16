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
