'use strict';

const state = {
  settings: null,
  library: null,
  currentView: 'all',
  currentCategory: null,
  search: '',
  sort: 'newest',
  pendingCategoryRenameName: null,
  pendingMoveHash: null,
  pendingMoveRelativePath: null,
  pendingImportPaths: null,
  pointerDrag: null,
  virtualImages: null,
  virtualStart: 0,
  virtualEnd: 0,
  cardHeight: null,
  scrollFrameRequested: false,
  // True while the library is being scanned/loaded, so the UI can show an intentional
  // spinner instead of the misleading "no data" empty states before the first scan lands.
  loading: false,
  // Analysis state
  analyzing: false,
  analysisQueue: [],   // [{type: 'text'|'nsfw'|'ocr'|'chunk'|'vision', force: bool}]
  analysisRunning: null, // 'text' | 'nsfw' | 'ocr' | 'chunk' | 'vision' | null
  autoRefresh: null,
  // Dashboard. Only the window and the capture read are held: every other figure is recomputed
  // from `library.images` on each paint, because a full pass over 50k records is ~3 ms and a cache
  // is a way for the numbers to be one category assignment out of date.
  dashDays: 90,
  dashCaptures: undefined,
  dashCapturesLoading: false,
  // The nightly run happening in the OTHER process right now, polled from a state file, or null
  // when none is. Nothing else in this window can see it — see the banner section.
  autoRun: null,
  // What the next scheduled run would find waiting for it, per folder. Read on demand rather than
  // polled: it is a statement about the last scan of each folder, and nothing between two opens of
  // this tab can change it except a run, which reloads it on the way out.
  autoQueue: null,
  chunkPlan: null,
  // Geo layer (own sidecars, independent of categories). `geoSetImages` holds the resolved images
  // of an opened country set — an explicit ordered member list, not a filter over the library.
  geoSummary: null,
  geoCoverage: null,
  geoSets: null,
  // Extracted-text search. `textStatus` is what the panel paints before anything is queried; it is
  // read without ever building, so opening the tab never starts work. Everything here mirrors what
  // `icat` does from a terminal — one index, two front ends.
  textStatus: null,
  textMode: 'images',
  textHits: null,
  textBuckets: null,
  textMatched: 0,
  textUnknownTerms: [],
  textSelectedHash: null,
  textDetail: null,
  textBusy: false,
  textQueryToken: 0,
  // Topic layer. `topicRun` is non-null only while a run is in flight; the last line it wrote stays
  // on screen afterwards, so a finished run is still readable.
  topicStatus: null,
  topicRun: null,
  topicMessage: null,
  // The gazetteer's `overrides` table, so the worklist can show what each string was decided to
  // mean — including decisions made by hand-editing the file. `geoOverrideBusy` holds the one
  // location string currently being written, so only its own row goes inert.
  geoOverrides: null,
  geoOverrideBusy: null,
  // Worklist rows expanded to show the frames behind them, and the frames themselves once fetched.
  // Cached per location string so collapsing and re-opening a row costs nothing — the descriptions
  // read behind it is the expensive half.
  geoWorklistOpen: new Set(),
  geoWorklistImages: new Map(),
  geoSetImages: null,
  geoSetTitle: null,
  geoBusy: false,
  // Freshness of the vintages on the Geo panel, plus proof the panel actually re-read them.
  // `geoCheckedAt` is stamped on every load so the toolbar can say WHEN it last looked — without it
  // a refresh that changed nothing is indistinguishable from a refresh that never happened.
  // `geoStatus.fingerprint` is what a focus check compares against to notice a write made by
  // another app (super-image-viewer owns the exclusion list) while this window was in the background.
  geoStatus: null,
  geoCheckedAt: null,
  geoRefreshing: false,
  // The one long process the panel's parts were collapsed into. `steps` is the chosen run, each
  // entry carrying its own state and a one-line result, so the strip can say what a finished part
  // actually did rather than just that it finished. Kept after the run ends — the last result is
  // the thing you come back to read.
  geoPipeline: { steps: [], active: false, cancelled: false },
  // Bridges the event-driven scene pass into the sequential runner: held while a classify step is
  // in flight, resolved by the finished event.
  kindWaiter: null,
  // Scene-kind classification (the "is this a place at all" pass).
  kindSummary: null,
  kindRunning: false,
  kindProgress: null,
  // Post-build set review. `geoReviewSelected` holds finding ids, not fixes, so a refreshed review
  // re-resolves what each tick means instead of applying a fix computed against older records.
  geoReview: null,
  geoReviewSelected: new Set(),
  geoReviewBusy: false,
  // Browser-style view history. Entries are view DESCRIPTORS plus the scroll offsets they were
  // left at — never snapshots of the images, so going Back re-resolves the view against the
  // current library instead of resurrecting a deleted image or a rebuilt set.
  nav: { entries: [], index: -1, restoring: false },
};

const els = {
  brand: document.querySelector('.brand'),
  navButtons: document.getElementById('nav-buttons'),
  navBackButton: document.getElementById('nav-back-button'),
  navForwardButton: document.getElementById('nav-forward-button'),
  dashboardTab: document.getElementById('dashboard-tab'),
  dashView: document.getElementById('dash-view'),
  dashRange: document.getElementById('dash-range'),
  dashRefreshButton: document.getElementById('dash-refresh-button'),
  dashAsOf: document.getElementById('dash-asof'),
  dashTiles: document.getElementById('dash-tiles'),
  dashUsage: document.getElementById('dash-usage'),
  dashOwn: document.getElementById('dash-own'),
  dashActivity: document.getElementById('dash-activity'),
  dashCaptures: document.getElementById('dash-captures'),
  dashCoverage: document.getElementById('dash-coverage'),
  dashContents: document.getElementById('dash-contents'),
  dashFolders: document.getElementById('dash-folders'),
  allTab: document.getElementById('all-tab'),
  allCount: document.getElementById('all-count'),
  unclassifiedTab: document.getElementById('unclassified-tab'),
  unclassifiedCount: document.getElementById('unclassified-count'),
  geoTab: document.getElementById('geo-tab'),
  geoCount: document.getElementById('geo-count'),
  textTab: document.getElementById('text-tab'),
  textCount: document.getElementById('text-count'),
  automationTab: document.getElementById('automation-tab'),
  automationCount: document.getElementById('automation-count'),
  autoView: document.getElementById('auto-view'),
  autoStateChip: document.getElementById('auto-state-chip'),
  autoRunNowButton: document.getElementById('auto-run-now-button'),
  autoStopButton: document.getElementById('auto-stop-button'),
  autoRecheckButton: document.getElementById('auto-recheck-button'),
  autoCounted: document.getElementById('auto-counted'),
  autoLive: document.getElementById('auto-live'),
  autoLiveLabel: document.getElementById('auto-live-label'),
  autoLiveLimit: document.getElementById('auto-live-limit'),
  autoLiveTrack: document.getElementById('auto-live-track'),
  autoLiveFill: document.getElementById('auto-live-fill'),
  autoLiveDetail: document.getElementById('auto-live-detail'),
  autoFacts: document.getElementById('auto-facts'),
  autoQueue: document.getElementById('auto-queue'),
  openAutomationButton: document.getElementById('open-automation-button'),
  textView: document.getElementById('text-view'),
  textQuery: document.getElementById('text-query'),
  textFrom: document.getElementById('text-from'),
  textTo: document.getElementById('text-to'),
  textRangePreset: document.getElementById('text-range-preset'),
  textSearchButton: document.getElementById('text-search-button'),
  textRebuildButton: document.getElementById('text-rebuild-button'),
  textRefreshButton: document.getElementById('text-refresh-button'),
  textModeImages: document.getElementById('text-mode-images'),
  textModeBuckets: document.getElementById('text-mode-buckets'),
  textBucketHours: document.getElementById('text-bucket-hours'),
  textIncludeDupes: document.getElementById('text-include-dupes'),
  textRequireAll: document.getElementById('text-require-all'),
  textStatusLine: document.getElementById('text-status-line'),
  textTopicsButton: document.getElementById('text-topics-button'),
  textTopicsStop: document.getElementById('text-topics-stop'),
  textTopicsProgress: document.getElementById('text-topics-progress'),
  textCoverage: document.getElementById('text-coverage'),
  captureActivity: document.getElementById('capture-activity'),
  textResults: document.getElementById('text-results'),
  textDetail: document.getElementById('text-detail'),
  textActions: document.getElementById('text-actions'),
  textActionsLabel: document.getElementById('text-actions-label'),
  textActionCategory: document.getElementById('text-action-category'),
  textCategorizeButton: document.getElementById('text-categorize-button'),
  textCopyButton: document.getElementById('text-copy-button'),
  geoView: document.getElementById('geo-view'),
  geoStats: document.getElementById('geo-stats'),
  geoLegend: document.getElementById('geo-legend'),
  geoClusters: document.getElementById('geo-clusters'),
  geoSets: document.getElementById('geo-sets'),
  geoWorklist: document.getElementById('geo-worklist'),
  geoCountryOptions: document.getElementById('geo-country-options'),
  geoGenerated: document.getElementById('geo-generated'),
  geoKinds: document.getElementById('geo-kinds'),
  geoReview: document.getElementById('geo-review'),
  geoGazetteerButton: document.getElementById('geo-gazetteer-button'),
  geoRefreshButton: document.getElementById('geo-refresh-button'),
  geoRunButton: document.getElementById('geo-run-button'),
  geoStopButton: document.getElementById('geo-stop-button'),
  geoPipeline: document.getElementById('geo-pipeline'),
  geoStepDerive: document.getElementById('geo-step-derive'),
  geoStepClassify: document.getElementById('geo-step-classify'),
  geoStepRepropagate: document.getElementById('geo-step-repropagate'),
  geoStepBuild: document.getElementById('geo-step-build'),
  geoStepReview: document.getElementById('geo-step-review'),
  geoFreshness: document.getElementById('geo-freshness'),
  geoSetSize: document.getElementById('geo-set-size'),
  categoryList: document.getElementById('category-list'),
  addCategoryButton: document.getElementById('add-category-button'),
  sourceFolderList: document.getElementById('source-folder-list'),
  addSourceFolderButton: document.getElementById('add-source-folder-button'),
  rootFolderSelect: document.getElementById('root-folder-select'),
  viewTitle: document.getElementById('view-title'),
  viewSubtitle: document.getElementById('view-subtitle'),
  imageGrid: document.getElementById('image-grid'),
  emptyState: document.getElementById('empty-state'),
  loadingState: document.getElementById('loading-state'),
  loadingLabel: document.getElementById('loading-label'),
  statusSpinner: document.getElementById('status-spinner'),
  main: document.querySelector('.main'),
  sidebarToggle: document.getElementById('sidebar-toggle'),
  mainDropTarget: document.getElementById('main-drop-target'),
  searchInput: document.getElementById('search-input'),
  statusMessage: document.getElementById('status-message'),
  statusDismiss: document.getElementById('status-dismiss'),
  sortSelect: document.getElementById('sort-select'),
  refreshButton: document.getElementById('refresh-button'),
  analyzeButton: document.getElementById('analyze-button'),
  reanalyzeButton: document.getElementById('reanalyze-button'),
  cancelAnalysisButton: document.getElementById('cancel-analysis-button'),
  analyzeTextCheck: document.getElementById('analyze-text-check'),
  analyzeNsfwCheck: document.getElementById('analyze-nsfw-check'),
  extractTextCheck: document.getElementById('extract-text-check'),
  analyzeChunkCheck: document.getElementById('analyze-chunk-check'),
  analyzeVisionCheck: document.getElementById('analyze-vision-check'),
  analyzeNsfwCheckLabel: document.getElementById('analyze-nsfw-check-label'),
  analysisPending: document.getElementById('analysis-pending'),
  openFolderButton: document.getElementById('open-folder-button'),
  settingsButton: document.getElementById('settings-button'),
  categoryDialog: document.getElementById('category-dialog'),
  categoryForm: document.getElementById('category-form'),
  categoryNameInput: document.getElementById('category-name-input'),
  cancelCategoryButton: document.getElementById('cancel-category-button'),
  categoryRenameDialog: document.getElementById('category-rename-dialog'),
  categoryRenameForm: document.getElementById('category-rename-form'),
  categoryRenameInput: document.getElementById('category-rename-input'),
  cancelCategoryRenameButton: document.getElementById('cancel-category-rename-button'),
  importButton: document.getElementById('import-button'),
  importDialog: document.getElementById('import-dialog'),
  importCount: document.getElementById('import-count'),
  importFolderSelect: document.getElementById('import-folder-select'),
  importNewFolderInput: document.getElementById('import-new-folder-input'),
  importForm: document.getElementById('import-form'),
  cancelImportButton: document.getElementById('cancel-import-button'),
  importFolderButton: document.getElementById('import-folder-button'),
  dropOverlay: document.getElementById('drop-overlay'),
  moveDialog: document.getElementById('move-dialog'),
  moveForm: document.getElementById('move-form'),
  moveFolderSelect: document.getElementById('move-folder-select'),
  moveNewFolderInput: document.getElementById('move-new-folder-input'),
  cancelMoveButton: document.getElementById('cancel-move-button'),
  dialogScrim: document.getElementById('dialog-scrim'),
  titlebarMinimize: document.getElementById('titlebar-minimize'),
  titlebarMaximize: document.getElementById('titlebar-maximize'),
  titlebarClose: document.getElementById('titlebar-close'),
  resizeGrips: document.getElementById('resize-grips'),
  settingsDialog: document.getElementById('settings-dialog'),
  settingsForm: document.getElementById('settings-form'),
  settingsRootFolder: document.getElementById('settings-root-folder'),
  settingsRootButton: document.getElementById('settings-root-button'),
  sourcePatternPreset: document.getElementById('source-pattern-preset'),
  sourcePatternRegex: document.getElementById('source-pattern-regex'),
  manualFolderList: document.getElementById('manual-folder-list'),
  tileSizeInput: document.getElementById('tile-size-input'),
  tileSizeValue: document.getElementById('tile-size-value'),
  darkModeInput: document.getElementById('dark-mode-input'),
  windowDefaultsStatus: document.getElementById('window-defaults-status'),
  saveWindowDefaultsButton: document.getElementById('save-window-defaults-button'),
  clearWindowDefaultsButton: document.getElementById('clear-window-defaults-button'),
  ocrWordThresholdInput: document.getElementById('ocr-word-threshold-input'),
  ocrWordThresholdValue: document.getElementById('ocr-word-threshold-value'),
  ocrAreaThresholdInput: document.getElementById('ocr-area-threshold-input'),
  ocrAreaThresholdValue: document.getElementById('ocr-area-threshold-value'),
  nsfwThresholdInput: document.getElementById('nsfw-threshold-input'),
  nsfwThresholdValue: document.getElementById('nsfw-threshold-value'),
  nsfwModelHint: document.getElementById('nsfw-model-hint'),
  downloadNsfwModelButton: document.getElementById('download-nsfw-model-button'),
  nsfwModelReport: document.getElementById('nsfw-model-report'),
  autoRefreshEnabledInput: document.getElementById('auto-refresh-enabled-input'),
  autoRefreshOptions: document.getElementById('auto-refresh-options'),
  autoRefreshHour: document.getElementById('auto-refresh-hour'),
  autoRefreshMinute: document.getElementById('auto-refresh-minute'),
  autoRefreshRootList: document.getElementById('auto-refresh-root-list'),
  autoRefreshNsfwInput: document.getElementById('auto-refresh-nsfw-input'),
  autoRefreshTextAnalysisInput: document.getElementById('auto-refresh-text-analysis-input'),
  autoRefreshTextExtractionInput: document.getElementById('auto-refresh-text-extraction-input'),
  autoRefreshVisionInput: document.getElementById('auto-refresh-vision-input'),
  autoRefreshVisionMinutesInput: document.getElementById('auto-refresh-vision-minutes-input'),
  autoRefreshGpuWaitInput: document.getElementById('auto-refresh-gpu-wait-input'),
  autoRefreshLowPriorityInput: document.getElementById('auto-refresh-low-priority-input'),
  autoRefreshToastInput: document.getElementById('auto-refresh-toast-input'),
  autoRunBanner: document.getElementById('auto-run-banner'),
  autoRunTitle: document.getElementById('auto-run-title'),
  autoRunDetail: document.getElementById('auto-run-detail'),
  autoRunTrack: document.getElementById('auto-run-track'),
  autoRunFill: document.getElementById('auto-run-fill'),
  autoRunLimit: document.getElementById('auto-run-limit'),
  autoRunStop: document.getElementById('auto-run-stop'),
  visionEndpointInput: document.getElementById('vision-endpoint-input'),
  visionModelInput: document.getElementById('vision-model-input'),
  visionModelSelect: document.getElementById('vision-model-select'),
  visionModelStatus: document.getElementById('vision-model-status'),
  refreshVisionModelsButton: document.getElementById('refresh-vision-models-button'),
  loadVisionModelButton: document.getElementById('load-vision-model-button'),
  visionApiKeyInput: document.getElementById('vision-api-key-input'),
  visionIdleUnloadInput: document.getElementById('vision-idle-unload-input'),
  visionIdleMinutesInput: document.getElementById('vision-idle-minutes-input'),
  visionIdleStatus: document.getElementById('vision-idle-status'),
  chunkPlanStatus: document.getElementById('chunk-plan-status'),
  regenerateChunkPlanButton: document.getElementById('regenerate-chunk-plan-button'),
  openChunkPlanButton: document.getElementById('open-chunk-plan-button'),
  discardChunkPlanButton: document.getElementById('discard-chunk-plan-button'),
  toast: document.getElementById('toast'),
  toastText: document.getElementById('toast-text'),
  toastDismiss: document.getElementById('toast-dismiss'),
};

// Every toast now carries a ✕ and sticks around long enough to actually be read — the results
// that matter ("Geo derived — 7614 images across 53 countries", an import summary) arrive at the
// end of a long run, when you're looking anywhere but the corner of the screen. 2.4s lost them.
// `sticky` (errors) additionally wraps its text, makes it selectable/copyable, and stays longer.
const TOAST_MS = 15000;
// Deliberately left at 30s: a sticky toast is the one that takes pointer events over the whole box
// (so its text can be selected), and stretching that further would block the grid corner underneath.
const TOAST_STICKY_MS = 30000;

function showToast(message, { sticky = false } = {}) {
  els.toastText.textContent = message;
  els.toast.classList.add('visible');
  els.toast.classList.toggle('sticky', sticky);
  clearTimeout(showToast.timer);
  showToast.timer = setTimeout(dismissToast, sticky ? TOAST_STICKY_MS : TOAST_MS);
}

function dismissToast() {
  clearTimeout(showToast.timer);
  els.toast.classList.remove('visible', 'sticky');
}

// Same reasoning as the toast: a terminal status ("Analysis complete.") is the one line telling
// you a long pass finished, so it holds for 45s and offers a ✕ rather than blinking out in 5s.
// `persist` keeps the message up until it's explicitly replaced — used for in-progress messages
// like "Scanning…", which can outlast any timeout on a large library and would otherwise vanish
// mid-scan, making the app look stalled.
const STATUS_MS = 45000;

function setStatus(message, persist = false) {
  els.statusMessage.textContent = message || '';
  els.statusMessage.title = message || '';
  // The ✕ only exists to clear a message, so it must not linger once there is nothing to clear.
  els.statusDismiss.hidden = !message;
  clearTimeout(setStatus.timer);
  if (message && !persist) {
    setStatus.timer = setTimeout(clearStatus, STATUS_MS);
  }
}

function clearStatus() {
  setStatus('');
}

// The small topbar spinner signals background activity for both a library scan and an
// analysis pass, so it's driven off both flags rather than tied to one caller.
function updateActivityIndicator() {
  const active = state.loading || state.analyzing;
  els.statusSpinner.classList.toggle('active', active);
}

function setLoading(active, label) {
  state.loading = active;
  if (active && label) els.loadingLabel.textContent = label;
  updateActivityIndicator();
}

function errorText(error) {
  return error?.message || String(error);
}

function shortPath(path) {
  if (!path) return '';
  const normalized = path.replace(/\//g, '\\');
  const parts = normalized.split('\\').filter(Boolean);
  if (parts.length <= 3) return normalized;
  return `${parts[0]}\\...\\${parts.slice(-2).join('\\')}`;
}

function formatBytes(bytes) {
  if (!bytes) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

function formatDate(ms) {
  if (!ms) return 'Unknown date';
  return new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  }).format(new Date(ms));
}

function imageCountLabel(count) {
  return `${count} image${count === 1 ? '' : 's'}`;
}

function tileSize() {
  return Number(state.settings?.tileSize || 168);
}

function applyUiSettings() {
  const settings = state.settings;
  if (!settings) return;
  document.body.classList.toggle('light-mode', !settings.darkMode);
  document.documentElement.style.setProperty('--tile-size', `${tileSize()}px`);
}

function syncSettingsDialog() {
  const settings = state.settings;
  const library = state.library;
  if (!settings) return;
  els.settingsRootFolder.textContent = library?.root || settings.lastRoot || 'No folder chosen';
  els.settingsRootFolder.title = library?.root || settings.lastRoot || '';
  els.tileSizeInput.value = String(tileSize());
  els.tileSizeValue.textContent = `${tileSize()}px`;
  els.darkModeInput.checked = settings.darkMode;
  els.sourcePatternPreset.value = library?.sourcePatternPreset || '';
  els.sourcePatternRegex.value = library?.sourcePatternRegex || '';
  const wordThreshold = library?.ocrWordThreshold ?? 35;
  const areaThresholdPercent = Math.round((library?.ocrAreaThreshold ?? 0.05) * 100);
  els.ocrWordThresholdInput.value = String(wordThreshold);
  els.ocrWordThresholdValue.textContent = `${wordThreshold} words`;
  els.ocrAreaThresholdInput.value = String(areaThresholdPercent);
  els.ocrAreaThresholdValue.textContent = `${areaThresholdPercent}%`;
  const nsfwPct = Math.round((library?.nsfwScoreThreshold ?? 0.45) * 100);
  els.nsfwThresholdInput.value = String(nsfwPct);
  els.nsfwThresholdValue.textContent = `${nsfwPct}%`;
  renderManualFolderList();
}

async function syncNsfwModelHint() {
  try {
    const info = await window.categorizerAPI.getNsfwModelInfo();
    if (info.exists) {
      els.nsfwModelHint.textContent = `Model loaded: ${info.path}`;
      els.nsfwModelHint.style.color = '';
      els.downloadNsfwModelButton.classList.add('hidden');
    } else {
      els.nsfwModelHint.textContent =
        `Model not installed. Press Download Model to install 320n.onnx to: ${info.path}`;
      els.nsfwModelHint.style.color = 'var(--danger)';
      els.downloadNsfwModelButton.classList.remove('hidden');
      els.downloadNsfwModelButton.disabled = false;
    }
    els.analyzeNsfwCheckLabel.title = info.exists
      ? 'Run NudeNet explicit content detection, classifying images as Safe / Explicit'
      : `NudeNet model not found — download 320n.onnx to: ${info.path}`;
  } catch {
    // non-fatal
  }
}

async function downloadNsfwModel() {
  els.downloadNsfwModelButton.disabled = true;
  els.nsfwModelHint.textContent = 'Downloading NudeNet package from PyPI...';
  els.nsfwModelHint.style.color = '';
  els.nsfwModelReport.textContent = [
    'Download started.',
    'Source: NudeNet 3.4.2 PyPI wheel',
    'Next: extract bundled nudenet/320n.onnx',
  ].join('\n');
  els.nsfwModelReport.classList.remove('hidden');
  try {
    const result = await window.categorizerAPI.downloadNsfwModel();
    const info = result.info;
    els.nsfwModelHint.textContent = `Model loaded: ${info.path}`;
    els.downloadNsfwModelButton.classList.add('hidden');
    els.nsfwModelReport.textContent = [
      'Download complete.',
      `Package: ${formatBytes(result.downloadedBytes)}`,
      `Model: ${formatBytes(result.modelBytes)}`,
      `Installed: ${info.path}`,
      `Source: ${result.sourceUrl}`,
      '',
      ...(result.report || []),
    ].join('\n');
    showToast('NudeNet model installed.');
  } catch (error) {
    els.downloadNsfwModelButton.disabled = false;
    const message = errorText(error);
    els.nsfwModelHint.textContent = 'Download failed. See report below.';
    els.nsfwModelHint.style.color = 'var(--danger)';
    els.nsfwModelReport.textContent = [
      'Download failed.',
      message,
      '',
      'No model was installed. You can try Download Model again.',
    ].join('\n');
    showToast(message);
  }
}

function formatChunkPlanStatus(plan) {
  if (!plan || !plan.exists) return 'No chunk plan yet — run "Video Dedup" under Analyze to build one.';
  const gen = plan.generatedAt ? ` · built ${formatDate(Date.parse(plan.generatedAt))}` : '';
  const g = plan.groups === 1 ? '' : 's';
  const f = plan.totalFrames === 1 ? '' : 's';
  return `${plan.groups} video${g} · ${plan.totalFrames} frame${f} · ${plan.selectedFrames} selected for description${gen}`;
}

async function loadVisionAndChunkSettings() {
  try {
    const vision = await window.categorizerAPI.getVisionSettings();
    els.visionEndpointInput.value = vision.endpoint || '';
    els.visionModelInput.value = vision.model || '';
    els.visionApiKeyInput.value = vision.apiKey || '';
    els.visionIdleUnloadInput.checked = vision.idleUnload !== false;
    els.visionIdleMinutesInput.value = vision.idleMinutes ?? 5;
  } catch {
    // non-fatal
  }
  // Fetch the model list in the background so an unreachable endpoint never delays the dialog.
  refreshVisionModels();
  refreshVisionIdleStatus();
  try {
    state.chunkPlan = state.library?.root
      ? await window.categorizerAPI.getChunkPlan(state.library.root)
      : null;
  } catch {
    state.chunkPlan = null;
  }
  els.chunkPlanStatus.textContent = formatChunkPlanStatus(state.chunkPlan);
}

async function saveVisionSettings() {
  try {
    await window.categorizerAPI.setVisionSettings(
      els.visionEndpointInput.value.trim(),
      els.visionModelInput.value.trim(),
      els.visionApiKeyInput.value.trim(),
      els.visionIdleUnloadInput.checked,
      Number(els.visionIdleMinutesInput.value) || 5,
    );
  } catch (error) {
    showToast(errorText(error));
  }
  refreshVisionIdleStatus();
}

// Says whether the app is currently holding a model open, which is the one thing about the idle
// lease a user can't infer from the settings — the rest happens inside LM Studio.
async function refreshVisionIdleStatus() {
  if (!els.visionIdleStatus) return;
  try {
    const status = await window.categorizerAPI.getVisionIdleStatus();
    if (!status.enabled) {
      els.visionIdleStatus.textContent = 'Off — a model this app loads stays until LM Studio’s own timeout.';
    } else if (!status.ownedModel) {
      els.visionIdleStatus.textContent =
        `Idle release after ${status.idleMinutes} min. Nothing held right now — this app hasn’t loaded a model.`;
    } else {
      els.visionIdleStatus.textContent =
        `Holding “${status.ownedModel}” — it will unload after ${status.idleMinutes} min with no requests from any app and no activity here.`;
    }
  } catch {
    els.visionIdleStatus.textContent = '';
  }
}

// The backend lets a model it loaded unload after a quiet spell, and "quiet" has to include the
// user — someone sorting images between passes must not have the model pulled out from under them.
// Only deliberate interaction counts: mousemove would make an abandoned window with the cursor
// parked over it look busy forever. Throttled to one call a half-minute, because the backend only
// needs this accurate to the minute.
const APP_ACTIVITY_THROTTLE_MS = 30000;
let lastActivityReportAt = 0;

function reportAppActivity() {
  const now = Date.now();
  if (now - lastActivityReportAt < APP_ACTIVITY_THROTTLE_MS) return;
  lastActivityReportAt = now;
  window.categorizerAPI.noteAppActivity().catch(() => {});
}

function installAppActivityHeartbeat() {
  // Capture phase, so a handler that stops propagation deeper in the app can't hide activity.
  for (const type of ['pointerdown', 'keydown', 'wheel']) {
    window.addEventListener(type, reportAppActivity, { passive: true, capture: true });
  }
  // Coming back to the window is activity too. Both are wired because neither fires reliably alone
  // in WebView2 — the same pairing the geo panel's self-reload needs.
  window.addEventListener('focus', reportAppActivity);
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) reportAppActivity();
  });
  // Keep the Settings line honest when the model goes while the dialog is open.
  window.categorizerAPI.onModelLeaseReleased(() => refreshVisionIdleStatus()).catch(() => {});
}

// Populates the model dropdown from the endpoint's /v1/models. Uses new Option(text, value) so ids
// are set as text (no HTML injection). The free-text input stays the source of truth for the saved
// model — the dropdown just fills it — so a custom id that isn't in the list still works.
async function refreshVisionModels() {
  const select = els.visionModelSelect;
  if (!select) return;
  const current = els.visionModelInput.value.trim();
  els.visionModelStatus.textContent = 'Loading model list…';
  try {
    const models = await window.categorizerAPI.listVisionModels();
    select.replaceChildren(new Option('— pick a model —', ''));
    for (const id of models) select.add(new Option(id, id));
    // Only select a saved model the server actually offers. Assigning an unknown value doesn't
    // no-op — it sets selectedIndex to -1, leaving the dropdown blank instead of on its placeholder.
    if (current && models.includes(current)) select.value = current;
    els.visionModelStatus.textContent = models.length
      ? `${models.length} model${models.length === 1 ? '' : 's'} available.`
      : 'Server reachable but no models listed — download or enable one in LM Studio.';
  } catch (error) {
    select.replaceChildren(new Option('— couldn’t reach endpoint —', ''));
    els.visionModelStatus.textContent = errorText(error);
  }
}

// Actively load the picked model into LM Studio so Describe won't fail against an empty server.
async function loadSelectedVisionModel() {
  const model = (els.visionModelInput.value.trim() || els.visionModelSelect.value).trim();
  if (!model) {
    showToast('Pick or type a model first.');
    return;
  }
  els.loadVisionModelButton.disabled = true;
  els.visionModelStatus.textContent = `Loading “${model}” — a cold model can take a while…`;
  try {
    const message = await window.categorizerAPI.loadVisionModel(model);
    els.visionModelStatus.textContent = message;
    showToast(message);
  } catch (error) {
    const text = errorText(error);
    els.visionModelStatus.textContent = text;
    showToast(text, { sticky: true });
  } finally {
    els.loadVisionModelButton.disabled = false;
  }
}

async function regenerateChunkPlan() {
  if (!state.library?.root) return;
  try {
    state.chunkPlan = await window.categorizerAPI.regenerateChunkPlan(state.library.root);
    els.chunkPlanStatus.textContent = formatChunkPlanStatus(state.chunkPlan);
    showToast('Chunk plan regenerated.');
  } catch (error) {
    showToast(errorText(error));
  }
}

async function discardChunkPlan() {
  if (!state.library?.root) return;
  if (!window.confirm('Discard the video chunk plan? Describe will then consider every image, with no video-frame de-duplication.')) return;
  try {
    state.chunkPlan = await window.categorizerAPI.discardChunkPlan(state.library.root);
    els.chunkPlanStatus.textContent = formatChunkPlanStatus(state.chunkPlan);
    showToast('Chunk plan discarded.');
  } catch (error) {
    showToast(errorText(error));
  }
}

async function openChunkPlanFile() {
  if (!state.chunkPlan?.exists) {
    showToast('No chunk plan file yet — run "Video Dedup" first.');
    return;
  }
  try {
    await window.categorizerAPI.revealImage(state.chunkPlan.path);
  } catch (error) {
    showToast(errorText(error));
  }
}

function renderAutoRefreshRootList() {
  const knownRoots = state.settings?.knownRoots || [];
  const selected = new Set(state.autoRefresh?.roots || []);
  els.autoRefreshRootList.innerHTML = '';
  if (!knownRoots.length) {
    const empty = document.createElement('div');
    empty.className = 'manual-folder-empty';
    empty.textContent = 'No known root folders yet — choose a root folder first.';
    els.autoRefreshRootList.append(empty);
    return;
  }
  for (const entry of knownRoots) {
    const row = document.createElement('label');
    row.className = 'auto-refresh-root-row';
    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    checkbox.value = entry.path;
    checkbox.checked = selected.has(entry.path);
    checkbox.addEventListener('change', saveAutoRefreshSettings);
    const label = document.createElement('span');
    label.textContent = entry.path;
    label.title = entry.path;
    row.append(checkbox, label);
    els.autoRefreshRootList.append(row);
  }
}

const DEFAULT_AUTO_REFRESH_TIME = '04:00';

// `<input type="time">` is not an option here: WebView2 formats it from the BROWSER's UI locale,
// which is en-US on this machine, so it renders "04:00 AM" — and setting `lang` on the document or
// on the element changes nothing (measured). Two selects state the 24-hour clock outright, and
// they have the second virtue of being unable to hold a time that does not exist.
function fillTimeSelects() {
  if (els.autoRefreshHour.options.length) return;
  const pad = value => String(value).padStart(2, '0');
  for (let hour = 0; hour < 24; hour += 1) els.autoRefreshHour.append(new Option(pad(hour), pad(hour)));
  for (let minute = 0; minute < 60; minute += 1) els.autoRefreshMinute.append(new Option(pad(minute), pad(minute)));
}

function setAutoRefreshTime(time) {
  fillTimeSelects();
  const [hour, minute] = String(time || DEFAULT_AUTO_REFRESH_TIME).split(':');
  els.autoRefreshHour.value = (hour || '04').padStart(2, '0');
  els.autoRefreshMinute.value = (minute || '00').padStart(2, '0');
}

// Falls back to the stored time rather than to a constant, for the same reason the vision-minutes
// box does: a select that somehow holds nothing must not quietly reschedule the run to midnight.
function autoRefreshTimeValue() {
  const stored = String(state.autoRefresh?.time || DEFAULT_AUTO_REFRESH_TIME).split(':');
  return `${els.autoRefreshHour.value || stored[0]}:${els.autoRefreshMinute.value || stored[1] || '00'}`;
}

function syncAutoRefreshControls() {
  const autoRefresh = state.autoRefresh;
  if (!autoRefresh) return;
  els.autoRefreshEnabledInput.checked = autoRefresh.enabled;
  setAutoRefreshTime(autoRefresh.time);
  els.autoRefreshNsfwInput.checked = autoRefresh.runNsfw;
  els.autoRefreshTextAnalysisInput.checked = autoRefresh.runTextAnalysis;
  els.autoRefreshTextExtractionInput.checked = autoRefresh.runTextExtraction;
  els.autoRefreshVisionInput.checked = autoRefresh.runVision;
  els.autoRefreshVisionMinutesInput.value = String(autoRefresh.visionMinutes ?? 30);
  els.autoRefreshGpuWaitInput.checked = autoRefresh.gpuWait;
  els.autoRefreshLowPriorityInput.checked = autoRefresh.lowPriority;
  els.autoRefreshToastInput.checked = autoRefresh.toast;
  els.autoRefreshOptions.classList.toggle('disabled-section', !autoRefresh.enabled);
  renderAutoRefreshRootList();
}

async function loadAutoRefreshSettings() {
  try {
    state.autoRefresh = await window.categorizerAPI.getAutoRefreshSettings();
  } catch (error) {
    state.autoRefresh = null;
    showToast(errorText(error));
  }
  syncAutoRefreshControls();
  if (state.currentView === 'automation') renderAutoPanel();
}

function collectCheckedAutoRefreshRoots() {
  return [...els.autoRefreshRootList.querySelectorAll('input[type="checkbox"]:checked')].map(input => input.value);
}

const VISION_MINUTES_MAX = 1440;

// Falls back to whatever the backend last reported rather than to a constant, so a half-typed or
// cleared box can never silently rewrite the limit — least of all to 0, which means "no limit".
function normalizeVisionMinutes(raw) {
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isFinite(parsed) || parsed < 0) {
    return state.autoRefresh?.visionMinutes ?? 30;
  }
  return Math.min(parsed, VISION_MINUTES_MAX);
}

async function saveAutoRefreshSettings() {
  const payload = {
    enabled: els.autoRefreshEnabledInput.checked,
    time: autoRefreshTimeValue(),
    roots: collectCheckedAutoRefreshRoots(),
    runNsfw: els.autoRefreshNsfwInput.checked,
    runTextAnalysis: els.autoRefreshTextAnalysisInput.checked,
    runTextExtraction: els.autoRefreshTextExtractionInput.checked,
    runVision: els.autoRefreshVisionInput.checked,
    // An empty or non-numeric box means "leave it as it was", not 0 — 0 is the explicit
    // no-limit value and must only ever come from someone actually typing it.
    visionMinutes: normalizeVisionMinutes(els.autoRefreshVisionMinutesInput.value),
    gpuWait: els.autoRefreshGpuWaitInput.checked,
    lowPriority: els.autoRefreshLowPriorityInput.checked,
    toast: els.autoRefreshToastInput.checked,
  };
  els.autoRefreshOptions.classList.toggle('disabled-section', !payload.enabled);
  try {
    state.autoRefresh = await window.categorizerAPI.setAutoRefreshSettings(payload);
  } catch (error) {
    showToast(errorText(error));
  }
  // Which folders and which passes are exactly what the queue counts, and the panel shows the two
  // a scroll apart — so a toggle re-answers the queue rather than leaving the old number standing.
  await loadAutoRefreshQueue();
}

function renderManualFolderList() {
  const library = state.library;
  els.manualFolderList.innerHTML = '';
  const manualFolders = (library?.sourceFolders || []).filter(folder => folder.isManual);
  if (!manualFolders.length) {
    const empty = document.createElement('div');
    empty.className = 'manual-folder-empty';
    empty.textContent = 'No manually added folders yet.';
    els.manualFolderList.append(empty);
    return;
  }
  for (const folder of manualFolders) {
    const row = document.createElement('div');
    row.className = 'manual-folder-row';
    const label = document.createElement('span');
    label.textContent = folder.name;
    const removeButton = document.createElement('button');
    removeButton.type = 'button';
    removeButton.className = 'button compact secondary';
    removeButton.textContent = 'Remove';
    removeButton.addEventListener('click', () => removeManualSourceFolder(folder.name));
    row.append(label, removeButton);
    els.manualFolderList.append(row);
  }
}

function includedSourceFolderNames() {
  const folders = state.library?.sourceFolders || [];
  if (!folders.length) return null;
  return new Set(folders.filter(folder => folder.includedInAnalysis).map(folder => folder.name));
}

function imagesInIncludedSourceFolders(images = state.library?.images || []) {
  const included = includedSourceFolderNames();
  if (!included) return images;
  return images.filter(image => included.has(image.sourceFolder));
}

function categoryCountsForIncludedSources() {
  const counts = new Map();
  let unclassified = 0;
  for (const image of imagesInIncludedSourceFolders()) {
    if (image.category) {
      counts.set(image.category, (counts.get(image.category) || 0) + 1);
    } else {
      unclassified += 1;
    }
  }
  return { counts, unclassified };
}

function visibleImages() {
  const library = state.library;
  if (!library) return [];

  // A geo set is an explicit, ordered member list, not a filter over the library: it deliberately
  // skips the source-folder filter and the sort, because both its membership and its order were
  // decided when the set was built.
  if (state.currentView === 'geoSet') {
    const images = state.geoSetImages || [];
    const setQuery = state.search.trim().toLowerCase();
    if (!setQuery) return images;
    return images.filter(image => `${image.name} ${image.relativePath}`.toLowerCase().includes(setQuery));
  }

  let images = imagesInIncludedSourceFolders(library.images);
  if (state.currentView === 'unclassified') {
    images = images.filter(image => !image.category);
  } else if (state.currentView === 'category') {
    images = images.filter(image => image.category === state.currentCategory);
  }

  const query = state.search.trim().toLowerCase();
  if (query) {
    images = images.filter(image => `${image.name} ${image.relativePath}`.toLowerCase().includes(query));
  }

  images = [...images];
  images.sort((a, b) => {
    if (state.sort === 'name') {
      return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });
    }
    if (state.sort === 'size') {
      return b.size - a.size || a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });
    }
    return b.modifiedMs - a.modifiedMs || a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });
  });

  return images;
}

function renderRootFolderSelect() {
  const knownRoots = state.settings?.knownRoots || [];
  const currentRoot = state.library?.root || state.settings?.lastRoot || '';

  els.rootFolderSelect.innerHTML = '';
  if (!knownRoots.length) {
    const empty = document.createElement('option');
    empty.value = '';
    empty.textContent = 'No folder chosen';
    els.rootFolderSelect.append(empty);
  }
  for (const entry of knownRoots) {
    const option = document.createElement('option');
    option.value = entry.path;
    option.textContent = entry.exists ? shortPath(entry.path) : `${shortPath(entry.path)} (not found)`;
    els.rootFolderSelect.append(option);
  }
  const addOption = document.createElement('option');
  addOption.value = '__add__';
  addOption.textContent = '+ Add Folder...';
  els.rootFolderSelect.append(addOption);

  els.rootFolderSelect.value = currentRoot;
}

function renderSettings() {
  if (!state.settings) return;
  applyUiSettings();
  renderRootFolderSelect();
  syncSettingsDialog();
}

// Sidebar pills sit in a 280px column that also has to fit a category name and its controls, so a
// six-digit count is not a display problem but a layout one — it squeezes the name to nothing.
// Full precision below 100k, where the number is still something you read digit by digit; thousands
// above it, where only the magnitude is being read anyway. The exact figure goes in the tooltip.
function countLabel(value) {
  const count = Number(value) || 0;
  if (count < 100000) return String(count);
  // Rounded first, then range-checked: 999,999 rounds to 1000k, which is the one number this scale
  // must never print.
  const thousands = Math.round(count / 1000);
  if (thousands < 1000) return `${thousands}k`;
  return `${(count / 1000000).toFixed(count < 10000000 ? 1 : 0)}M`;
}

function setCountPill(element, value) {
  const count = Number(value) || 0;
  element.textContent = countLabel(count);
  element.title = count.toLocaleString();
}

function renderSidebar() {
  const library = state.library;
  const includedImages = imagesInIncludedSourceFolders();
  const { counts: categoryCounts, unclassified } = categoryCountsForIncludedSources();
  const allCount = includedImages.length;
  const unclassifiedCount = unclassified;

  setCountPill(els.allCount, allCount);
  setCountPill(els.unclassifiedCount, unclassifiedCount);
  els.dashboardTab.classList.toggle('active', state.currentView === 'dashboard');
  els.allTab.classList.toggle('active', state.currentView === 'all');
  els.unclassifiedTab.classList.toggle('active', state.currentView === 'unclassified');
  // An opened set still belongs to Geo, so the tab stays lit while browsing one.
  els.geoTab.classList.toggle('active', state.currentView === 'geo' || state.currentView === 'geoSet');
  setCountPill(els.geoCount, state.geoSummary?.stats?.taggedTotal || 0);
  els.textTab.classList.toggle('active', state.currentView === 'text');
  setCountPill(els.textCount, state.textStatus?.docs || 0);
  els.automationTab.classList.toggle('active', state.currentView === 'automation');
  // The pill is what the next scheduled run would process, so a schedule with nothing to do reads
  // as a calm 0 rather than as an unknown. `is-clear` is what stops that 0 looking like a fault.
  setCountPill(els.automationCount, state.autoQueue?.scheduledPending || 0);
  els.automationCount.classList.toggle('is-clear', (state.autoQueue?.scheduledPending || 0) === 0);

  els.categoryList.innerHTML = '';
  const categories = library?.categories || [];
  if (state.loading && !library) {
    const loadingEl = document.createElement('div');
    loadingEl.className = 'category-empty';
    loadingEl.textContent = 'Loading…';
    els.categoryList.append(loadingEl);
  } else if (!categories.length) {
    const empty = document.createElement('button');
    empty.className = 'category-empty';
    empty.type = 'button';
    empty.textContent = 'Add your first category';
    empty.disabled = state.analyzing;
    empty.addEventListener('click', openCategoryDialog);
    els.categoryList.append(empty);
  } else {
    for (const category of categories) {
      const row = document.createElement('div');
      row.className = 'category-row';
      row.classList.toggle('omitted', category.includedInAnalysis === false);
      row.dataset.categoryName = category.name;

      // Leading checkbox: untick to omit these already-categorized images from analysis (chiefly
      // Describe). Mirrors the per-folder include toggle; reuses its styles.
      const includeLabel = document.createElement('label');
      includeLabel.className = 'source-folder-include';
      includeLabel.title = 'Include this category in analysis. Untick to skip these images (e.g. omit High Text from Describe since OCR already covers it).';
      const includeCheckbox = document.createElement('input');
      includeCheckbox.type = 'checkbox';
      includeCheckbox.checked = category.includedInAnalysis !== false;
      includeCheckbox.disabled = state.analyzing;
      includeCheckbox.addEventListener('click', event => event.stopPropagation());
      includeCheckbox.addEventListener('change', () => setCategoryAnalysisIncluded(category.name, includeCheckbox.checked));
      includeLabel.append(includeCheckbox);

      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'category-item';
      button.classList.toggle('active', state.currentView === 'category' && state.currentCategory === category.name);
      button.innerHTML = '<span class="category-name"></span><span class="count-pill"></span>';
      button.querySelector('.category-name').textContent = category.name;
      button.title = category.name;
      setCountPill(button.querySelector('.count-pill'), categoryCounts.get(category.name) || 0);
      button.addEventListener('click', () => selectCategory(category.name));

      // Icons, not the words "Rename"/"Delete": those two labels cost 109px of a 232px row, which
      // is why every category name was ellipsed to four characters. The names are the thing being
      // read here; the actions are already explained by their tooltips.
      const renameButton = document.createElement('button');
      renameButton.type = 'button';
      renameButton.className = 'category-rename-button';
      renameButton.title = `Rename ${category.name}`;
      renameButton.setAttribute('aria-label', `Rename ${category.name}`);
      renameButton.textContent = '✎';
      renameButton.disabled = state.analyzing;
      renameButton.addEventListener('click', event => {
        event.stopPropagation();
        openCategoryRenameDialog(category.name);
      });

      const deleteButton = document.createElement('button');
      deleteButton.type = 'button';
      deleteButton.className = 'category-rename-button';
      deleteButton.title = `Delete ${category.name}`;
      deleteButton.setAttribute('aria-label', `Delete ${category.name}`);
      deleteButton.textContent = '✕';
      deleteButton.disabled = state.analyzing;
      deleteButton.addEventListener('click', event => {
        event.stopPropagation();
        deleteCategoryConfirm(category.name);
      });

      row.append(includeLabel, button, renameButton, deleteButton);
      els.categoryList.append(row);
    }
  }

  els.sourceFolderList.innerHTML = '';
  const sourceFolders = library?.sourceFolders || [];
  if (state.loading && !library) {
    const loadingEl = document.createElement('div');
    loadingEl.className = 'category-empty';
    loadingEl.textContent = 'Loading…';
    els.sourceFolderList.append(loadingEl);
  } else if (!sourceFolders.length) {
    const empty = document.createElement('div');
    empty.className = 'category-empty';
    empty.textContent = 'No source folders detected yet.';
    els.sourceFolderList.append(empty);
  } else {
    for (const folder of sourceFolders) {
      const row = document.createElement('div');
      row.className = 'source-folder-row';
      row.innerHTML = `
        <label class="source-folder-include" title="Include this folder in browsing and analysis">
          <input type="checkbox" class="source-folder-checkbox">
        </label>
        <span class="category-name"></span>
        <span class="count-pill"></span>
      `;
      const checkbox = row.querySelector('.source-folder-checkbox');
      checkbox.checked = folder.includedInAnalysis;
      checkbox.disabled = state.analyzing;
      checkbox.addEventListener('change', () => setFolderAnalysisIncluded(folder.name, checkbox.checked));
      row.querySelector('.category-name').textContent = folder.name;
      row.querySelector('.category-name').title = folder.name;
      setCountPill(row.querySelector('.count-pill'), folder.imageCount);
      els.sourceFolderList.append(row);
    }
  }
}

function renderHeader() {
  const library = state.library;
  if (state.currentView === 'dashboard') {
    els.viewTitle.textContent = 'Dashboard';
  } else if (state.currentView === 'all') {
    els.viewTitle.textContent = 'All Images';
  } else if (state.currentView === 'unclassified') {
    els.viewTitle.textContent = 'Unclassified';
  } else if (state.currentView === 'geo') {
    els.viewTitle.textContent = 'Geo Coverage';
  } else if (state.currentView === 'text') {
    els.viewTitle.textContent = 'Extracted Text';
  } else if (state.currentView === 'automation') {
    els.viewTitle.textContent = 'Automation';
  } else if (state.currentView === 'geoSet') {
    els.viewTitle.textContent = state.geoSetTitle || 'Country Set';
  } else {
    els.viewTitle.textContent = state.currentCategory || 'Category';
  }
  // While loading, prefer the known root path (reassures which folder is being scanned) over the
  // "no root" message, which is only true once a scan has actually landed with no folder set.
  els.viewSubtitle.textContent =
    library?.root ||
    (state.loading ? (state.settings?.lastRoot || 'Loading…') : 'No root folder chosen yet');
  // It shares its row with the title and the status line now, so a long root path ellipsizes —
  // the tooltip is where the whole thing stays readable.
  els.viewSubtitle.title = els.viewSubtitle.textContent;
}

// Built through `new Option(text, value)` rather than an interpolated HTML string: a name is
// free text, and `&`-sequences in one would be parsed as entities, so the option's value would
// stop matching the category the backend knows. Same reason as the vision-model dropdown.
function fillCategorySelect(select, selected) {
  const options = [new Option('Unclassified', '')];
  for (const category of state.library?.categories || []) {
    options.push(new Option(category.name, category.name, false, category.name === selected));
  }
  select.replaceChildren(...options);
}

function percent(value) {
  if (value == null || Number.isNaN(Number(value))) return null;
  return `${Math.round(Number(value) * 100)}%`;
}

function analysisSummary(image) {
  const lines = [];
  if (image.nsfwScore != null) {
    const threshold = state.library?.nsfwScoreThreshold ?? 0.45;
    const status = image.nsfwScore >= threshold ? 'Explicit' : 'Below explicit threshold';
    const labels = Array.isArray(image.nsfwLabels) && image.nsfwLabels.length
      ? image.nsfwLabels.join(' · ')
      : 'No NudeNet labels recorded';
    lines.push(`NudeNet: ${status} (${percent(image.nsfwScore)}; threshold ${percent(threshold)})`);
    lines.push(labels);
  } else {
    lines.push('NudeNet: not analyzed');
  }

  if (image.ocrWordCount != null && image.ocrTextAreaRatio != null) {
    lines.push(`Text: ${image.ocrWordCount} words · ${percent(image.ocrTextAreaRatio)} area`);
  } else {
    lines.push('Text: not analyzed');
  }

  if (image.ocrTextChars != null) {
    lines.push(image.ocrTextChars > 0
      ? `Extracted text: ${image.ocrTextChars} chars saved`
      : 'Extracted text: no text found');
  }

  if (image.videoTitle) {
    lines.push(`Video: ${image.videoTitle}`);
  }

  if (image.visionDescChars != null) {
    lines.push(image.visionDescChars > 0
      ? `Description: ${image.visionDescChars} chars saved`
      : 'Description: empty');
  }

  if (image.classifiedBy) {
    lines.push(`Classification: ${image.category || 'Unclassified'} (${image.classifiedBy})`);
  }

  return lines;
}

function buildImageCard(image) {
  const card = document.createElement('article');
  card.className = 'image-card';
  card.dataset.hash = image.hash;
  card.dataset.path = image.path;
  card.dataset.name = image.name;
  card.innerHTML = `
    <div class="thumbnail">
      <img class="thumb-image" alt="" loading="lazy">
    </div>
    <div class="card-main">
      <div class="file-title"></div>
      <div class="file-meta"></div>
      <div class="analysis-summary"></div>
      <div class="card-controls">
        <select class="category-select"></select>
      </div>
      <div class="card-actions">
        <button class="button compact ghost open-button" type="button">Open</button>
        <button class="button compact ghost reveal-button" type="button">Show</button>
      </div>
    </div>
  `;

  card.querySelector('.thumb-image').src = window.categorizerAPI.getFileUrl(image.thumbnailPath || image.path);
  card.querySelector('.file-title').textContent = image.name;
  const folderText = image.sourceFolder ? ` · ${image.sourceFolder}` : '';
  const classifiedBy = image.classifiedBy;
  const badge = classifiedBy === 'manual' ? ' · manual'
    : (classifiedBy === 'auto' || classifiedBy === 'auto-nsfw') ? ' · auto'
    : '';
  const nsfwBadge = image.nsfwScore != null
    ? ` · ${Math.round(image.nsfwScore * 100)}% explicit`
    : '';
  card.querySelector('.file-meta').textContent =
    `${formatDate(image.modifiedMs)} · ${formatBytes(image.size)}${folderText}${nsfwBadge}${badge}`;
  const summaryLines = analysisSummary(image);
  const summary = card.querySelector('.analysis-summary');
  summary.textContent = summaryLines.join('\n');
  summary.title = summaryLines.join('\n');

  const select = card.querySelector('.category-select');
  fillCategorySelect(select, image.category);
  select.disabled = state.analyzing;
  select.addEventListener('change', () => assignCategory(image.hash, select.value || null));

  card.querySelector('.open-button').addEventListener('click', () => openImage(image.path));
  card.querySelector('.reveal-button').addEventListener('click', () => revealImage(image.path));
  card.addEventListener('pointerdown', event => startPointerDrag(event, card));

  return card;
}

const GRID_ROW_GAP = 14;
const VIRTUAL_BUFFER_ROWS = 3;

function estimatedCardHeight() {
  return state.cardHeight || tileSize() + 92;
}

function computeGridColumns() {
  const gridWidth = els.imageGrid.clientWidth || els.mainDropTarget.clientWidth;
  const tile = tileSize();
  return Math.max(1, Math.floor((gridWidth + GRID_ROW_GAP) / (tile + GRID_ROW_GAP)));
}

function computeVirtualWindow(totalImages) {
  const columns = computeGridColumns();
  const rowHeight = estimatedCardHeight() + GRID_ROW_GAP;
  const totalRows = Math.ceil(totalImages / columns);
  const scrollTop = els.mainDropTarget.scrollTop;
  const viewportHeight = els.mainDropTarget.clientHeight;
  const bufferPx = rowHeight * VIRTUAL_BUFFER_ROWS;

  const startRow = Math.max(0, Math.floor((scrollTop - bufferPx) / rowHeight));
  const endRow = Math.min(totalRows, Math.ceil((scrollTop + viewportHeight + bufferPx) / rowHeight));

  return {
    startIndex: startRow * columns,
    endIndex: Math.min(totalImages, endRow * columns),
    topPadding: startRow * rowHeight,
    bottomPadding: (totalRows - endRow) * rowHeight,
  };
}

function renderVirtualWindow(images, window_) {
  state.virtualImages = images;
  state.virtualStart = window_.startIndex;
  state.virtualEnd = window_.endIndex;
  els.imageGrid.style.paddingTop = `${window_.topPadding}px`;
  els.imageGrid.style.paddingBottom = `${window_.bottomPadding}px`;
  els.imageGrid.innerHTML = '';

  for (const image of images.slice(window_.startIndex, window_.endIndex)) {
    els.imageGrid.append(buildImageCard(image));
  }

  if (!state.cardHeight) {
    const firstCard = els.imageGrid.querySelector('.image-card');
    if (firstCard) state.cardHeight = firstCard.getBoundingClientRect().height;
  }
}

function onGridScroll() {
  if (state.scrollFrameRequested) return;
  state.scrollFrameRequested = true;
  requestAnimationFrame(() => {
    state.scrollFrameRequested = false;
    const images = state.virtualImages;
    if (!images || !images.length) return;
    const window_ = computeVirtualWindow(images.length);
    if (window_.startIndex === state.virtualStart && window_.endIndex === state.virtualEnd) return;
    renderVirtualWindow(images, window_);
  });
}

function renderImages() {
  const images = visibleImages();
  // Only take over the whole content area with the spinner when there's nothing to show yet.
  // A rescan that already has images keeps the grid visible (topbar spinner covers it), so the
  // view never blanks out and never feels locked up.
  const showLoading = state.loading && !images.length;
  els.loadingState.classList.toggle('hidden', !showLoading);
  els.emptyState.classList.toggle('visible', images.length === 0 && !showLoading);

  if (!images.length) {
    state.virtualImages = null;
    els.imageGrid.style.paddingTop = '0px';
    els.imageGrid.style.paddingBottom = '0px';
    els.imageGrid.innerHTML = '';
    if (showLoading) return;
    if (!state.library?.root) {
      els.emptyState.innerHTML = '';
      const button = document.createElement('button');
      button.className = 'button';
      button.type = 'button';
      button.textContent = 'Choose Root Folder';
      button.addEventListener('click', changeRootFolder);
      els.emptyState.append('No root folder chosen yet. ', button);
    } else {
      els.emptyState.textContent = state.search
        ? 'No images match that search.'
        : 'No images found yet. Add a monthly folder or a manual source folder, then Rescan.';
    }
    return;
  }

  renderVirtualWindow(images, computeVirtualWindow(images.length));
}

function render() {
  renderSettings();
  renderSidebar();
  renderHeader();
  renderNavButtons();
  renderAnalysisPending();
  // The coverage scoreboard swaps places with the image grid rather than rendering inside it, so
  // the grid's virtual scrolling and drop handling never see any of this.
  const geoActive = state.currentView === 'geo';
  const textActive = state.currentView === 'text';
  const autoActive = state.currentView === 'automation';
  const dashActive = state.currentView === 'dashboard';
  els.mainDropTarget.classList.toggle('hidden', geoActive || textActive || autoActive || dashActive);
  els.geoView.classList.toggle('hidden', !geoActive);
  els.textView.classList.toggle('hidden', !textActive);
  els.autoView.classList.toggle('hidden', !autoActive);
  els.dashView.classList.toggle('hidden', !dashActive);
  // Lets the stylesheet compact the header for this view: the search box and the sort order act on
  // the image grid, which is exactly what this view replaces, so they are inert here and the panel
  // gets their rows back. The text panel has its own query box for the same reason.
  document.body.classList.toggle('view-geo', geoActive);
  document.body.classList.toggle('view-text', textActive);
  document.body.classList.toggle('view-automation', autoActive);
  document.body.classList.toggle('view-dashboard', dashActive);
  if (geoActive) renderGeo();
  else if (textActive) renderText();
  else if (autoActive) renderAutoPanel();
  else if (dashActive) renderDashboard();
  else renderImages();
}

async function loadSettings() {
  state.settings = await window.categorizerAPI.getSettings();
}

async function refreshLibrary() {
  if (!state.settings?.lastRoot) {
    state.library = null;
    return;
  }
  try {
    state.library = await window.categorizerAPI.scanLibrary(state.settings.lastRoot);
  } catch (error) {
    showToast(errorText(error));
    state.library = null;
  }
}

async function refreshAll() {
  // A scan doesn't just read — `scan_and_reconcile` saves the sidecar back. A running analysis pass
  // rewrites that same file wholesale from the snapshot it took when it started, so a scan landing
  // mid-pass gets erased, exactly as an import would. The Rescan button is disabled while analyzing,
  // but Ctrl+R reaches this directly — so the guard belongs here, on the one path they share.
  if (state.analyzing) {
    showToast('Analysis is running — wait for it to finish before rescanning.');
    return;
  }
  setLoading(true);
  setStatus('Scanning for new, moved, or deleted images…', true);
  // Paint the loading state before the (potentially slow) scan begins — otherwise the first
  // intentional frame wouldn't land until the scan already finished.
  render();
  try {
    await loadSettings();
    render();
    await refreshLibrary();
  } catch (error) {
    showToast(errorText(error));
    state.settings = {
      lastRoot: null,
      lastRootExists: false,
      tileSize: 168,
      darkMode: true,
      knownRoots: [],
    };
    state.library = null;
  }
  setLoading(false);
  if (state.currentView === 'category' && !(state.library?.categories || []).some(c => c.name === state.currentCategory)) {
    const gone = state.currentCategory;
    state.currentView = 'all';
    state.currentCategory = null;
    pruneNavEntries(entry => !(entry.view === 'category' && entry.category === gone));
    pushNavEntry(navEntry('all'));
  }
  render();
  // Geo is a read of three small sidecars, so the sidebar tally can be filled in without making
  // the scan wait on it.
  if (state.library) void loadGeoSummaryOnly();
  // A rescan rewrote the very records the scheduled queue is counted from, so its numbers are now
  // one scan out of date — including the moment a Rescan is what clears the last of the backlog.
  void loadAutoRefreshQueue();
  if (state.library) {
    setStatus(`Up to date — ${imageCountLabel(state.library.images.length)}.`);
  } else if (state.settings?.lastRoot) {
    setStatus('Could not load the selected folder.');
  } else {
    setStatus('');
  }
}

function selectAll() {
  cancelPointerDrag();
  pushNavEntry(navEntry('all'));
  state.currentView = 'all';
  state.currentCategory = null;
  render();
}

function selectUnclassified() {
  cancelPointerDrag();
  pushNavEntry(navEntry('unclassified'));
  state.currentView = 'unclassified';
  state.currentCategory = null;
  render();
}

function selectCategory(name) {
  cancelPointerDrag();
  pushNavEntry(navEntry('category', { category: name }));
  state.currentView = 'category';
  state.currentCategory = name;
  render();
}

// ==============================
// View history — Back / Forward
//
// The friction this exists for: opening a country set out of a 200-row list is one click, and
// getting back to the same place in that list was none — re-entering Geo Coverage rebuilds the
// list at the top, so the row you came from has to be hunted down again. So an entry carries the
// scroll offsets of the surfaces it was left at, and restoring one puts them back.
//
// An entry is a descriptor, not a snapshot: a set's MEMBERS are re-fetched on the way back, and an
// entry whose category or set no longer exists is dropped rather than restored, because these
// views outlive the data behind them (a rebuild mints new set ids).
// ==============================

const NAV_HISTORY_LIMIT = 100;

function navEntry(view, extra = {}) {
  return {
    view,
    category: extra.category ?? null,
    geoSetId: extra.geoSetId ?? null,
    geoSetTitle: extra.geoSetTitle ?? null,
    scroll: { main: 0, geo: 0, geoSets: 0 },
  };
}

function navKey(entry) {
  return `${entry.view}|${entry.category || ''}|${entry.geoSetId || ''}`;
}

function currentNavEntry() {
  return state.nav.entries[state.nav.index] || null;
}

function navEntryLabel(entry) {
  if (!entry) return null;
  if (entry.view === 'all') return 'All Images';
  if (entry.view === 'unclassified') return 'Unclassified';
  if (entry.view === 'geo') return 'Geo Coverage';
  if (entry.view === 'text') return 'Extracted Text';
  if (entry.view === 'automation') return 'Automation';
  if (entry.view === 'dashboard') return 'Dashboard';
  if (entry.view === 'geoSet') return entry.geoSetTitle || 'country set';
  return entry.category || 'category';
}

// Offsets live on the entry rather than in one map keyed by view, because the same view can sit in
// the history twice at two different offsets — Geo Coverage before and after a detour through a set.
function captureNavScroll() {
  const entry = currentNavEntry();
  if (!entry) return;
  // A hidden surface reports 0; only overwrite what is actually on screen, or opening a set from
  // Geo Coverage would record the grid's 0 over the set list's real offset.
  const geoActive = state.currentView === 'geo';
  const textActive = state.currentView === 'text';
  const autoActive = state.currentView === 'automation';
  const dashActive = state.currentView === 'dashboard';
  entry.scroll = {
    main: geoActive || textActive || autoActive || dashActive ? entry.scroll.main : els.mainDropTarget.scrollTop,
    geo: geoActive ? els.geoView.scrollTop : entry.scroll.geo,
    geoSets: geoActive ? els.geoSets.scrollTop : entry.scroll.geoSets,
  };
}

function pushNavEntry(entry) {
  if (state.nav.restoring) return;
  captureNavScroll();
  const current = currentNavEntry();
  if (current && navKey(current) === navKey(entry)) {
    // Re-selecting the view you are already on is not a navigation: keep the entry, its offsets,
    // and the forward tail. Only the label can have gone stale (a set rebuilt at a new size).
    current.geoSetTitle = entry.geoSetTitle ?? current.geoSetTitle;
    renderNavButtons();
    return;
  }
  state.nav.entries.length = state.nav.index + 1;   // a new branch drops the forward tail
  state.nav.entries.push(entry);
  if (state.nav.entries.length > NAV_HISTORY_LIMIT) state.nav.entries.shift();
  state.nav.index = state.nav.entries.length - 1;
  renderNavButtons();
}

// Switching root folder means a different library entirely — every entry describes a place in the
// old one, so the trail starts over rather than pointing at categories that may not exist here.
function resetNavHistory(view = 'all') {
  state.nav.entries = [navEntry(view)];
  state.nav.index = 0;
  renderNavButtons();
}

// Deleting a category leaves entries pointing at something gone. Dropping them beats letting Back
// land on an error. Adjacent duplicates are collapsed so the removal doesn't leave a dead press.
function pruneNavEntries(keep) {
  const entries = [];
  let index = 0;
  state.nav.entries.forEach((entry, i) => {
    if (!keep(entry)) return;
    const previous = entries[entries.length - 1];
    if (!previous || navKey(previous) !== navKey(entry)) entries.push(entry);
    if (i <= state.nav.index) index = entries.length - 1;
  });
  state.nav.entries = entries;
  state.nav.index = entries.length ? Math.min(index, entries.length - 1) : -1;
  renderNavButtons();
}

function renderNavButtons() {
  const back = state.nav.entries[state.nav.index - 1] || null;
  const forward = state.nav.entries[state.nav.index + 1] || null;
  els.navBackButton.disabled = !back;
  els.navForwardButton.disabled = !forward;
  els.navBackButton.title = back
    ? `Back to ${navEntryLabel(back)} (Alt+← or Mouse 4)`
    : 'Back (Alt+← or Mouse 4)';
  els.navForwardButton.title = forward
    ? `Forward to ${navEntryLabel(forward)} (Alt+→ or Mouse 5)`
    : 'Forward (Alt+→ or Mouse 5)';
}

// Applied after layout: the grid is virtualised, so its full scroll height only exists once the
// padded window has been rendered, and assigning scrollTop before that would clamp to nothing.
function restoreNavScroll(entry) {
  requestAnimationFrame(() => {
    const scroll = entry.scroll || {};
    els.mainDropTarget.scrollTop = scroll.main || 0;
    els.geoView.scrollTop = scroll.geo || 0;
    els.geoSets.scrollTop = scroll.geoSets || 0;
    // The grid only rendered the rows around offset 0; the scroll event that would widen the
    // virtual window fires after this frame, so ask for the recompute directly.
    onGridScroll();
  });
}

async function applyNavEntry(entry) {
  const root = state.library?.root || state.settings?.lastRoot;
  // Held across the whole restore, not just the render: re-reading a set's members takes a
  // second or two on a large library, and a second press landing in the middle of that would
  // move the index out from under the navigation already in flight.
  state.nav.restoring = true;
  try {
    if (entry.view === 'category' && !(state.library?.categories || []).some(c => c.name === entry.category)) {
      showToast(`“${entry.category}” no longer exists.`);
      pruneNavEntries(e => !(e.view === 'category' && e.category === entry.category));
      // Step past it to whatever survived, so a dead entry costs one press rather than swallowing
      // it. Terminates: every pass removes at least the entry it was handed.
      const next = currentNavEntry();
      if (next) await applyNavEntry(next);
      return;
    }

    let images = null;
    if (entry.view === 'geoSet') {
      // Members come off disk on every open, so this is visibly not instant — say what is
      // happening, exactly as opening the set from the list does.
      setStatus(`Opening ${entry.geoSetTitle || 'set'}…`, true);
      try {
        images = root ? await window.categorizerAPI.getGeoSetImages(root, entry.geoSetId) : null;
      } catch {
        images = null;
      } finally {
        setStatus('');
      }
      if (!images) {
        // Sets are rebuilt with fresh ids, so a set from before a rebuild is gone for good:
        // replace the entry with the coverage view it was opened from, rather than leaving a
        // dead step in the trail.
        showToast('That set has been rebuilt — showing Geo Coverage.');
        const replacement = navEntry('geo');
        replacement.scroll = entry.scroll;
        state.nav.entries[state.nav.index] = replacement;
        entry = replacement;
      }
    }

    state.currentView = entry.view;
    state.currentCategory = entry.view === 'category' ? entry.category : null;
    if (entry.view === 'geoSet') {
      state.geoSetImages = images;
      state.geoSetTitle = entry.geoSetTitle;
    }
    render();
    restoreNavScroll(entry);
    if (entry.view === 'geo') {
      // The panel paints from cache first and again when the sidecars land — the second paint
      // rebuilds the set list from scratch, taking its scroll offset back to 0 with it.
      await loadGeoData();
      restoreNavScroll(entry);
    }
  } finally {
    state.nav.restoring = false;
  }
}

async function navigateBy(delta) {
  if (state.nav.restoring) return;
  const target = state.nav.index + delta;
  if (target < 0 || target >= state.nav.entries.length) return;
  cancelPointerDrag();
  captureNavScroll();
  state.nav.index = target;
  renderNavButtons();
  await applyNavEntry(state.nav.entries[state.nav.index]);
  renderNavButtons();
}

function installNavShortcuts() {
  els.navBackButton.addEventListener('click', () => navigateBy(-1));
  els.navForwardButton.addEventListener('click', () => navigateBy(1));

  // The thumb buttons are the point of the feature — this app is browsed like a browser, and that
  // is where the hand already is. Captured on `mousedown`, which is where WebView2 would otherwise
  // start a history navigation of its own inside the webview; cancelling `auxclick` as well stops
  // the release landing as a click on whatever button is under the cursor.
  const NAV_MOUSE_DELTAS = { 3: -1, 4: 1 };
  for (const type of ['mousedown', 'mouseup', 'auxclick']) {
    window.addEventListener(type, event => {
      const delta = NAV_MOUSE_DELTAS[event.button];
      if (delta === undefined) return;
      event.preventDefault();
      if (type !== 'mousedown' || document.querySelector('dialog[open]')) return;
      navigateBy(delta);
    }, { capture: true });
  }
}

// ==============================
// Geo coverage
//
// A scoreboard against a fixed 109-country reference list, so a country you have NOTHING for still
// gets a row — showing what is missing is the whole point. Everything is counted in distinct videos
// rather than images, because variety is what decides whether a set is worth practising on.
// ==============================

const GEO_TIER_LABELS = {
  empty: 'Empty',
  seed: 'Seed (1-3)',
  thin: 'Thin (4-15)',
  ready: 'Ready (16+)',
  deep: 'Deep (32+)',
};

const GEO_TIER_ORDER = ['empty', 'seed', 'thin', 'ready', 'deep'];

function selectGeo() {
  cancelPointerDrag();
  pushNavEntry(navEntry('geo'));
  state.currentView = 'geo';
  state.currentCategory = null;
  render();
  // Fire-and-forget: the panel paints immediately from whatever is cached and fills in after.
  loadGeoData();
}

// Just the tally for the sidebar pill — cheap enough to run after every scan.
async function loadGeoSummaryOnly() {
  const root = state.library?.root;
  if (!root) return;
  try {
    state.geoSummary = await window.categorizerAPI.getGeoSummary(root);
    renderSidebar();
  } catch {
    // A library with no geo sidecar yet is the normal case, not an error worth surfacing.
  }
}

// Re-reads every sidecar the panel shows, in one pass, and stamps when that happened. Returns
// whether it worked, so a caller that wants to report the outcome does not announce a success the
// catch block just swallowed.
async function loadGeoData() {
  const root = state.library?.root || state.settings?.lastRoot;
  if (!root) return false;
  let ok = true;
  try {
    const [summary, coverage, sets, kinds, overrides, status] = await Promise.all([
      window.categorizerAPI.getGeoSummary(root),
      window.categorizerAPI.getGeoCoverage(root),
      window.categorizerAPI.getGeoSets(root),
      window.categorizerAPI.getKindSummary(root),
      window.categorizerAPI.getGeoOverrides(root),
      window.categorizerAPI.getGeoStatus(root),
    ]);
    state.geoSummary = summary;
    state.geoCoverage = coverage;
    state.geoSets = sets;
    state.kindSummary = kinds;
    state.geoOverrides = overrides;
    state.geoStatus = status;
    state.geoCheckedAt = Date.now();
  } catch (error) {
    ok = false;
    showToast(errorText(error));
  }
  render();
  return ok;
}

// The manual re-read. Everything on this panel comes off disk, three of the five files can be
// rewritten by something other than this window, and until now the only way to make it look again
// was to navigate away and back — which is why it seemed to refresh "at times".
async function refreshGeoData() {
  if (state.geoRefreshing || state.geoBusy) return;
  const before = state.geoStatus?.fingerprint ?? null;
  state.geoRefreshing = true;
  setStatus('Re-reading geo sidecars…', true);
  render();
  const ok = await loadGeoData();
  state.geoRefreshing = false;
  setStatus('');
  if (ok) {
    const after = state.geoStatus?.fingerprint ?? null;
    // "Nothing changed" is a real answer and has to be said, or a refresh that found nothing looks
    // exactly like a button that does nothing.
    showToast(before !== null && after === before
      ? `Nothing has changed on disk — ${geoContentsLabel()}.`
      : `Geo refreshed — ${geoContentsLabel()}.`);
  }
  render();
}

function geoContentsLabel() {
  const stats = state.geoCoverage?.stats;
  const sets = state.geoStatus?.setsCount ?? state.geoSets?.sets?.length ?? 0;
  if (!stats) return 'nothing derived yet';
  return `${stats.taggedTotal} images across ${stats.countriesSeen} countries, ` +
    `${sets} set${sets === 1 ? '' : 's'}`;
}

// The exclusion list is written by super-image-viewer and the gazetteer can be edited in a text
// editor — both while this window sits in the background showing what it last read. Coming back to
// the window is the moment to check, and the check costs one stat of five files plus a parse of the
// small ones, never the multi-megabyte records file.
async function checkGeoFreshness() {
  if (state.currentView !== 'geo') return;
  if (state.geoBusy || state.geoRefreshing || state.kindRunning || state.geoReviewBusy) return;
  const root = state.library?.root || state.settings?.lastRoot;
  if (!root) return;
  let status = null;
  try {
    status = await window.categorizerAPI.getGeoStatus(root);
  } catch {
    return; // a library that has gone away is not worth a toast on every focus
  }
  const changed = state.geoStatus && status.fingerprint !== state.geoStatus.fingerprint;
  state.geoStatus = status;
  state.geoCheckedAt = Date.now();
  if (!changed) {
    renderGeo();
    return;
  }
  // Read-only panel, so reloading under the user is safe — but it must be visible that it happened,
  // because the numbers moving on their own is otherwise indistinguishable from a bug.
  await loadGeoData();
  showToast('Geo sidecars changed on disk — reloaded.');
}

// The scene pass talks to the local model, so it is the one part that reports by event rather than
// returning. This wraps it back into something awaitable, so the runner can treat all five parts
// alike instead of special-casing the long one. Results checkpoint after every batch, so a Stop
// loses at most one batch.
function classifyScenes(root, step) {
  return new Promise((resolve, reject) => {
    state.kindWaiter = { resolve, reject, step };
    state.kindRunning = true;
    state.kindProgress = null;
    window.categorizerAPI.classifyKinds(root, false).catch(error => {
      state.kindWaiter = null;
      state.kindRunning = false;
      reject(error);
    });
  });
}

async function installKindListeners() {
  await window.categorizerAPI.onKindProgress(payload => {
    state.kindProgress = payload;
    // Feed the running chip too, so "which part is happening now" carries its own count.
    if (state.kindWaiter) state.kindWaiter.step.detail = `${payload.processed} / ${payload.total}`;
    if (state.currentView === 'geo') renderGeo();
    setStatus(`Classifying scenes — ${payload.processed}/${payload.total}`, true);
  });
  await window.categorizerAPI.onKindFinished(payload => {
    state.kindRunning = false;
    state.kindProgress = null;
    setStatus('');
    const waiter = state.kindWaiter;
    state.kindWaiter = null;
    if (!waiter) {
      if (payload?.message) showToast(payload.message);
      void loadGeoData();
      return;
    }
    // A failure has to stop the chain: building sets on a half-classified library would bake the
    // very guesses the classify step exists to remove.
    if (payload?.status === 'error') {
      waiter.reject(new Error(payload.message || 'Scene classification failed.'));
    } else {
      waiter.resolve(payload);
    }
  });
}

// ==============================
// The geo pipeline
//
// These five parts were five buttons, which made the common case — "bring everything up to date" —
// a five-click sequence you had to know the order of. They are a strict chain: each one's output is
// the next one's input, so the only genuine choice is how far along it to go. That is what the tick
// boxes are, and the order below is the dependency order, not a preference.
// ==============================

const GEO_PIPELINE_STEPS = [
  {
    id: 'derive',
    label: 'Derive',
    checkbox: 'geoStepDerive',
    status: 'Deriving geo from descriptions…',
    async run(root, step) {
      // A derive regenerates the worklist wholesale, so every expanded row and cached strip
      // describes a list that is about to stop existing.
      state.geoWorklistOpen.clear();
      state.geoWorklistImages.clear();
      state.geoSummary = await window.categorizerAPI.deriveGeo(root);
      const stats = state.geoSummary.stats;
      step.detail = `${stats.taggedTotal} images · ${stats.countriesSeen} countries`;
    },
  },
  {
    id: 'classify',
    label: 'Classify Scenes',
    checkbox: 'geoStepClassify',
    status: 'Classifying scenes…',
    cancellable: true,
    async run(root, step) {
      const outcome = await classifyScenes(root, step);
      if (outcome?.status === 'cancelled') {
        // A stop is a decision, not a failure — but the parts after this would run on a
        // half-classified library, so the chain ends here rather than baking that in.
        step.state = 'stopped';
        // The progress handler has been writing "n / total" into this all along, and how far it got
        // is the one thing worth keeping — `state.kindProgress` is already cleared by now.
        step.detail = step.detail ? `stopped at ${step.detail}` : 'stopped';
        state.geoPipeline.cancelled = true;
        return;
      }
      step.detail = outcome?.message || 'done';
    },
  },
  {
    id: 'repropagate',
    label: 'Re-propagate',
    checkbox: 'geoStepRepropagate',
    status: 'Re-deciding inherited scene kinds…',
    async run(root, step) {
      const result = await window.categorizerAPI.repropagateKinds(root);
      step.detail = `${result.mixed} held out as mixed` +
        (result.corrected ? ` · ${result.corrected} corrected` : '');
    },
  },
  {
    id: 'build',
    label: 'Build Sets',
    checkbox: 'geoStepBuild',
    status: 'Building country sets…',
    async run(root, step) {
      const targetSize = Math.max(4, Math.min(64, Number(els.geoSetSize.value) || 16));
      const built = await window.categorizerAPI.buildGeoSets(root, targetSize);
      state.geoSets = built;
      // Any review on screen argued about member lists that have just been replaced.
      state.geoReview = null;
      state.geoReviewSelected = new Set();
      const diverse = built.sets.filter(set => set.quality === 'diverse').length;
      step.detail = `${built.sets.length} sets · ${diverse} fully varied`;
    },
  },
  {
    id: 'review',
    label: 'Review',
    checkbox: 'geoStepReview',
    status: 'Reviewing country sets…',
    async run(root, step) {
      state.geoReviewBusy = true;
      try {
        state.geoReview = await window.categorizerAPI.reviewGeoSets(root);
        // Findings are re-resolved on every run, so a tick that no longer exists must not survive.
        const live = new Set(state.geoReview.findings.map(finding => finding.id));
        state.geoReviewSelected = new Set([...state.geoReviewSelected].filter(id => live.has(id)));
        step.detail = state.geoReview.findings.length
          ? `${state.geoReview.findings.length} findings`
          : 'nothing to fix';
      } finally {
        state.geoReviewBusy = false;
      }
    },
  },
];

function geoPipelineBusy() {
  return state.geoPipeline.active || state.geoBusy || state.kindRunning;
}

/// Runs the named parts in dependency order. Used both by the Run button and by the apply-fixes
/// chain, so there is exactly one thing that knows how these parts follow each other.
async function runGeoPipeline(ids) {
  const root = state.library?.root || state.settings?.lastRoot;
  if (!root || geoPipelineBusy()) return;
  const chosen = GEO_PIPELINE_STEPS.filter(step => ids.includes(step.id));
  if (!chosen.length) {
    showToast('Tick at least one part to run.');
    return;
  }

  state.geoPipeline = {
    steps: chosen.map(step => ({ id: step.id, label: step.label, state: 'pending', detail: '' })),
    active: true,
    cancelled: false,
  };
  state.geoBusy = true;
  render();

  let failed = null;
  for (let index = 0; index < chosen.length; index += 1) {
    const entry = state.geoPipeline.steps[index];
    if (state.geoPipeline.cancelled) {
      entry.state = 'skipped';
      continue;
    }
    entry.state = 'running';
    setStatus(`${index + 1}/${chosen.length} · ${chosen[index].status}`, true);
    render();
    try {
      await chosen[index].run(root, entry);
      // A step may name its own outcome (the scene pass does, when stopped part-way); anything
      // that simply returned is done.
      if (entry.state === 'running') entry.state = 'done';
    } catch (error) {
      entry.state = 'failed';
      entry.detail = errorText(error);
      failed = chosen[index].label;
      // Every later part reads what this one was supposed to write, so carrying on would produce
      // confident output from stale input — the exact failure this panel already has a banner for.
      state.geoPipeline.cancelled = true;
    }
    render();
  }

  state.geoPipeline.active = false;
  state.geoBusy = false;
  setStatus('');
  await loadGeoData();

  const done = state.geoPipeline.steps.filter(step => step.state === 'done').length;
  if (failed) {
    showToast(`Stopped at ${failed} — ${done} of ${state.geoPipeline.steps.length} parts finished.`, { sticky: true });
  } else if (state.geoPipeline.cancelled) {
    showToast(`Stopped — ${done} of ${state.geoPipeline.steps.length} parts finished.`);
  } else {
    showToast(`Done — ${state.geoPipeline.steps.map(step => `${step.label}: ${step.detail}`).join(' · ')}`);
  }
}

function runSelectedGeoPipeline() {
  const ids = GEO_PIPELINE_STEPS.filter(step => els[step.checkbox].checked).map(step => step.id);
  return runGeoPipeline(ids);
}

// Stop applies to the part actually running. Only the scene pass is interruptible; for the short
// ones this just prevents the parts after them from starting.
async function stopGeoPipeline() {
  if (!state.geoPipeline.active) return;
  state.geoPipeline.cancelled = true;
  setStatus('Stopping…', true);
  if (state.kindRunning) {
    try {
      await window.categorizerAPI.cancelKindClassification();
    } catch (error) {
      showToast(errorText(error));
    }
  }
  render();
}

const KIND_LABELS = {
  outdoor: 'outdoor — real places',
  indoor: 'indoor',
  person: 'people',
  screen: 'screens & maps',
  other: 'other',
  // Not a scene the model ever answers: a frame nobody looked at, in a video whose described frames
  // disagreed. Shown so the held-out population is visible rather than silently missing.
  mixed: 'mixed video — held out',
};

const KIND_ORDER = ['outdoor', 'indoor', 'person', 'screen', 'other', 'mixed'];

function renderGeoKinds() {
  els.geoKinds.innerHTML = '';
  const summary = state.kindSummary;
  if (!summary) return;

  if (state.kindRunning) {
    const progress = document.createElement('div');
    progress.className = 'geo-placeholder';
    progress.textContent = state.kindProgress
      ? `Classifying scenes — ${state.kindProgress.processed} of ${state.kindProgress.total}…`
      : 'Starting scene classification…';
    els.geoKinds.append(progress);
    return;
  }

  if (!summary.classified) {
    const hint = document.createElement('div');
    hint.className = 'geo-placeholder';
    hint.textContent = summary.pending
      ? `Scenes not classified yet — ${summary.pending} geo-tagged images to check. ` +
        'Until this runs, sets can include mall interiors, portraits and screenshots that happen to carry a country.'
      : 'Derive geo first, then classify scenes.';
    els.geoKinds.append(hint);
    return;
  }

  const row = document.createElement('div');
  row.className = 'geo-kind-row';
  const allowed = new Set(summary.allowedKinds || []);
  for (const kind of KIND_ORDER) {
    const count = summary.counts?.[kind] || 0;
    if (!count) continue;
    const chip = document.createElement('span');
    chip.className = `geo-kind-chip${allowed.has(kind) ? ' allowed' : ''}`;
    chip.textContent = `${KIND_LABELS[kind] || kind} ${count}`;
    chip.setAttribute(
      'aria-label',
      `${count} ${kind}${allowed.has(kind) ? ', used in sets' : ', kept out of sets'}`
    );
    row.append(chip);
  }
  els.geoKinds.append(row);

  const note = document.createElement('div');
  note.className = 'geo-placeholder';
  const kept = (summary.allowedKinds || []).join(', ');
  note.textContent = summary.pending
    ? `${summary.pending} still unclassified — those pass through until checked. Sets use: ${kept}.`
    : `Sets use: ${kept}. Edit "allowedKinds" in the kinds sidecar to change that without reclassifying.`;
  els.geoKinds.append(note);
}

async function openGeoSet(set) {
  const root = state.library?.root || state.settings?.lastRoot;
  if (!root) return;
  setStatus(`Opening ${set.title}…`, true);
  try {
    state.geoSetImages = await window.categorizerAPI.getGeoSetImages(root, set.id);
    state.geoSetTitle = `${set.title} — ${set.sources} video${set.sources === 1 ? '' : 's'}`;
    // Pushed only once the members are in hand, so a set that fails to open leaves no step to
    // walk back through. Recorded before the view switches, so the entry it leaves behind keeps
    // the set list's scroll offset — the whole point of Back here.
    pushNavEntry(navEntry('geoSet', { geoSetId: set.id, geoSetTitle: state.geoSetTitle }));
    state.currentView = 'geoSet';
  } catch (error) {
    showToast(errorText(error));
  } finally {
    setStatus('');
  }
  render();
}

function geoStatCard(label, value, hint) {
  const card = document.createElement('div');
  card.className = 'geo-stat';
  const valueEl = document.createElement('div');
  valueEl.className = 'geo-stat-value';
  valueEl.textContent = String(value);
  const labelEl = document.createElement('div');
  labelEl.className = 'geo-stat-label';
  labelEl.textContent = label;
  card.append(valueEl, labelEl);
  if (hint) card.setAttribute('aria-label', `${label}: ${value}. ${hint}`);
  return card;
}

function renderGeoStats() {
  els.geoStats.innerHTML = '';
  const coverage = state.geoCoverage;
  if (!coverage) {
    const hint = document.createElement('div');
    hint.className = 'geo-placeholder';
    hint.textContent = state.geoSummary?.exists === false
      ? 'No geo records yet. Press Derive Geo — it reads the descriptions already on disk and needs no model.'
      : 'Loading geo…';
    els.geoStats.append(hint);
    return;
  }
  const stats = coverage.stats;
  els.geoStats.append(
    geoStatCard('images tagged', stats.taggedTotal),
    geoStatCard('countries seen', stats.countriesSeen),
    geoStatCard('from own description', stats.taggedOwn),
    geoStatCard('from video propagation', stats.taggedPropagated),
    geoStatCard('unplaceable strings', stats.unresolvedStrings),
    geoStatCard('fiction videos skipped', stats.fictionGroupsSkipped)
  );
}

function renderGeoLegend() {
  els.geoLegend.innerHTML = '';
  const tiers = state.geoCoverage?.tiers || {};
  for (const tier of GEO_TIER_ORDER) {
    const item = document.createElement('span');
    item.className = `geo-legend-item tier-${tier}`;
    item.textContent = `${GEO_TIER_LABELS[tier]} · ${tiers[tier] || 0}`;
    els.geoLegend.append(item);
  }
}

function renderGeoClusters() {
  els.geoClusters.innerHTML = '';
  const coverage = state.geoCoverage;
  if (!coverage) return;

  for (const cluster of coverage.clusters) {
    const block = document.createElement('div');
    block.className = 'geo-cluster';

    const head = document.createElement('div');
    head.className = 'geo-cluster-head';
    const name = document.createElement('span');
    name.className = 'geo-cluster-name';
    name.textContent = cluster.name;
    const ratio = document.createElement('span');
    ratio.className = 'geo-cluster-ratio';
    ratio.classList.toggle('bare', cluster.ready === 0);
    ratio.textContent = `${cluster.ready}/${cluster.total} ready`;
    head.append(name, ratio);

    const row = document.createElement('div');
    row.className = 'geo-country-row';
    for (const country of cluster.countries) {
      const chip = document.createElement('span');
      chip.className = `geo-country tier-${country.tier}`;
      const label = document.createElement('span');
      label.className = 'geo-country-name';
      label.textContent = country.name;
      const count = document.createElement('span');
      count.className = 'geo-country-sources';
      count.textContent = country.sources ? String(country.sources) : '—';
      chip.append(label, count);
      if (country.delta > 0) {
        const delta = document.createElement('span');
        delta.className = 'geo-country-delta';
        delta.textContent = `+${country.delta}`;
        chip.append(delta);
      }
      chip.setAttribute(
        'aria-label',
        `${country.name}: ${country.sources} videos, ${country.images} images, ${country.tier}`
      );
      row.append(chip);
    }

    block.append(head, row);
    els.geoClusters.append(block);
  }
}

function renderGeoSets() {
  els.geoSets.innerHTML = '';
  const sets = state.geoSets?.sets || [];
  if (!sets.length) {
    const hint = document.createElement('div');
    hint.className = 'geo-placeholder';
    hint.textContent = state.geoCoverage
      ? 'No sets built yet. Press Build Country Sets.'
      : 'Derive geo first.';
    els.geoSets.append(hint);
    return;
  }

  for (const set of sets) {
    const row = document.createElement('button');
    row.type = 'button';
    row.className = 'geo-set';
    row.addEventListener('click', () => openGeoSet(set));

    const title = document.createElement('span');
    title.className = 'geo-set-title';
    title.textContent = set.title;

    const meta = document.createElement('span');
    meta.className = 'geo-set-meta';
    meta.textContent = `${set.size} images · ${set.sources} video${set.sources === 1 ? '' : 's'}`;

    const badge = document.createElement('span');
    badge.className = `geo-badge ${set.quality}`;
    badge.textContent = set.quality;

    row.append(title, meta, badge);
    els.geoSets.append(row);
  }
}

// ==============================
// Set review — the post-build cleanup pass
//
// Everything the forward pipeline gets wrong lands in a built set, and a set is looked at for weeks
// after it is built. This is the one place that walks what is on disk and argues against it, with
// the fix attached to each complaint. Nothing here edits records: a fix is a write to the exclusion
// list or the gazetteer, and it becomes permanent by re-deriving.
// ==============================

const GEO_REVIEW_KIND_LABELS = {
  'duplicate-video': 'Repeat frames of one video',
  'registry-port': 'Tagged off a ship’s flag',
  'wrong-kind': 'Not an allowed scene kind',
  people: 'People, not a place',
  unclassified: 'Never scene-classified',
  'single-video-country': 'Country rests on one video',
  'short-set': 'Set is short',
};

// Unions the ticked findings' fixes. Two findings that both want the same image excluded cost one
// write, and the backend is idempotent anyway — this just keeps the confirmation honest.
function selectedGeoReviewFix() {
  const excludeHashes = new Set();
  const rejectLocations = new Set();
  const fictionTitles = new Set();
  for (const finding of state.geoReview?.findings || []) {
    if (!state.geoReviewSelected.has(finding.id) || !finding.fix) continue;
    for (const hash of finding.fix.excludeHashes || []) excludeHashes.add(hash);
    for (const location of finding.fix.rejectLocations || []) rejectLocations.add(location);
    for (const title of finding.fix.fictionTitles || []) fictionTitles.add(title);
  }
  return {
    excludeHashes: [...excludeHashes],
    rejectLocations: [...rejectLocations],
    fictionTitles: [...fictionTitles],
  };
}

async function applyGeoReviewSelection() {
  const root = state.library?.root || state.settings?.lastRoot;
  const fixes = selectedGeoReviewFix();
  if (!root || state.geoReviewBusy) return;
  if (!fixes.excludeHashes.length && !fixes.rejectLocations.length && !fixes.fictionTitles.length) return;

  state.geoReviewBusy = true;
  setStatus('Applying fixes…', true);
  render();
  try {
    const applied = await window.categorizerAPI.applyGeoReview(root, fixes);
    showToast(
      `Applied — ${applied.excluded} image${applied.excluded === 1 ? '' : 's'} excluded, ` +
      `${applied.rejected} location string${applied.rejected === 1 ? '' : 's'} rejected. ` +
      'Re-derive and rebuild to see it in the sets.'
    );
    state.geoReviewSelected = new Set();
  } catch (error) {
    showToast(errorText(error));
  } finally {
    state.geoReviewBusy = false;
    setStatus('');
  }
  // Re-derive picks up the rejected strings; rebuilding picks up the exclusions. Run through the
  // pipeline rather than as its own private chain, so there is one thing that knows the order —
  // and so applying fixes reports its progress the same way every other run does.
  await runGeoPipeline(['derive', 'build', 'review']);
}

function geoReviewImageLine(image) {
  const row = document.createElement('div');
  row.className = 'geo-review-image';
  const name = document.createElement('span');
  name.className = 'geo-review-image-name';
  // The path is the only handle the user has on the actual file; the location string is what the
  // finding is arguing about. Both, in that order.
  name.textContent = image.path || image.hash;
  const why = document.createElement('span');
  why.className = 'geo-review-image-why';
  const parts = [];
  if (image.raw) parts.push(`“${image.raw}”`);
  if (image.kind) parts.push(image.kind);
  if (image.via) parts.push(image.via);
  why.textContent = parts.join(' · ');
  row.append(name, why);
  return row;
}

function renderGeoReviewFinding(finding) {
  const card = document.createElement('div');
  card.className = `geo-review-finding severity-${finding.severity}`;

  const head = document.createElement('div');
  head.className = 'geo-review-head';

  const label = document.createElement('label');
  label.className = 'geo-review-tick';
  const tick = document.createElement('input');
  tick.type = 'checkbox';
  tick.checked = state.geoReviewSelected.has(finding.id);
  tick.disabled = !finding.fix || state.geoReviewBusy;
  tick.addEventListener('change', () => {
    if (tick.checked) state.geoReviewSelected.add(finding.id);
    else state.geoReviewSelected.delete(finding.id);
    renderGeoReview();
  });
  const tickText = document.createElement('span');
  tickText.textContent = finding.fix ? finding.fix.label : 'No automatic fix';
  tickText.className = finding.fix ? '' : 'geo-review-nofix';
  label.append(tick, tickText);
  label.setAttribute(
    'aria-label',
    `${finding.setTitle}: ${finding.title}. ${finding.fix ? finding.fix.label : 'No automatic fix'}`
  );

  const heading = document.createElement('div');
  heading.className = 'geo-review-title';
  const where = document.createElement('span');
  where.className = 'geo-review-where';
  where.textContent = finding.setTitle;
  const what = document.createElement('span');
  what.className = 'geo-review-what';
  what.textContent = finding.title;
  heading.append(where, what);

  head.append(heading, label);

  const detail = document.createElement('p');
  detail.className = 'geo-review-detail';
  detail.textContent = finding.detail;

  card.append(head, detail);

  if (finding.images.length) {
    const list = document.createElement('div');
    list.className = 'geo-review-images';
    for (const image of finding.images.slice(0, 6)) list.append(geoReviewImageLine(image));
    if (finding.images.length > 6) {
      const more = document.createElement('div');
      more.className = 'geo-review-image-why';
      more.textContent = `… and ${finding.images.length - 6} more`;
      list.append(more);
    }
    card.append(list);
  }
  return card;
}

function renderGeoReview() {
  els.geoReview.innerHTML = '';
  const review = state.geoReview;
  els.geoReview.classList.toggle('hidden', !review && !state.geoReviewBusy);
  if (!review) {
    if (state.geoReviewBusy) {
      const busy = document.createElement('div');
      busy.className = 'geo-placeholder';
      busy.textContent = 'Reading every set member…';
      els.geoReview.append(busy);
    }
    return;
  }

  const head = document.createElement('div');
  head.className = 'geo-section-head';
  const title = document.createElement('h2');
  title.textContent = 'Set review';
  const hint = document.createElement('p');
  hint.className = 'geo-section-hint';
  hint.textContent =
    `${review.setsReviewed} sets, ${review.membersReviewed} members. Tick the fixes you agree with, ` +
    'then apply — every fix is a line in the exclusion list or the gazetteer, so re-deriving makes ' +
    'it permanent and deleting the line undoes it.';
  head.append(title, hint);
  els.geoReview.append(head);

  if (review.stale) {
    const warn = document.createElement('div');
    warn.className = 'geo-review-stale';
    warn.textContent = review.staleDetail;
    els.geoReview.append(warn);
  }

  if (!review.findings.length) {
    const clean = document.createElement('div');
    clean.className = 'geo-placeholder';
    clean.textContent = 'No findings — every set is one frame per video, every member is a place.';
    els.geoReview.append(clean);
    return;
  }

  const summary = document.createElement('div');
  summary.className = 'geo-review-summary';
  for (const [kind, count] of Object.entries(review.counts)) {
    const chip = document.createElement('span');
    chip.className = 'geo-review-chip';
    chip.textContent = `${GEO_REVIEW_KIND_LABELS[kind] || kind} · ${count}`;
    summary.append(chip);
  }
  els.geoReview.append(summary);

  const actions = document.createElement('div');
  actions.className = 'geo-review-actions';
  const fixable = review.findings.filter(finding => finding.fix);
  const allTicked = fixable.length > 0 && fixable.every(f => state.geoReviewSelected.has(f.id));

  const toggleAll = document.createElement('button');
  toggleAll.type = 'button';
  toggleAll.className = 'button secondary';
  toggleAll.textContent = allTicked ? 'Untick all' : `Tick all ${fixable.length} fixable`;
  toggleAll.disabled = state.geoReviewBusy || !fixable.length;
  toggleAll.addEventListener('click', () => {
    if (allTicked) state.geoReviewSelected.clear();
    else for (const finding of fixable) state.geoReviewSelected.add(finding.id);
    renderGeoReview();
  });

  const fixes = selectedGeoReviewFix();
  const apply = document.createElement('button');
  apply.type = 'button';
  apply.className = 'button';
  apply.textContent = fixes.excludeHashes.length || fixes.rejectLocations.length
    ? `Apply ${state.geoReviewSelected.size} fix${state.geoReviewSelected.size === 1 ? '' : 'es'} & rebuild`
    : 'Apply & rebuild';
  apply.disabled = state.geoReviewBusy || !state.geoReviewSelected.size;
  apply.addEventListener('click', () => void applyGeoReviewSelection());

  const note = document.createElement('span');
  note.className = 'geo-review-note';
  note.textContent = fixes.excludeHashes.length
    ? `${fixes.excludeHashes.length} images, ${fixes.rejectLocations.length} location strings`
    : 'Nothing ticked';

  actions.append(toggleAll, apply, note);
  els.geoReview.append(actions);

  const list = document.createElement('div');
  list.className = 'geo-review-list';
  for (const finding of review.findings) list.append(renderGeoReviewFinding(finding));
  els.geoReview.append(list);
}

// The decision recorded for a worklist string, or undefined if it has never been decided. The
// gazetteer keys on the lowercased string, exactly as the worklist reports it.
function geoDecisionFor(location) {
  const overrides = state.geoOverrides;
  if (!overrides) return undefined;
  const key = location.trim().toLowerCase();
  return Object.prototype.hasOwnProperty.call(overrides, key) ? overrides[key] : undefined;
}

// A decided string that is STILL on the worklist has not been through a derive yet — the worklist
// is regenerated wholesale on every derive, so its mere presence is the pending signal. No extra
// flag to keep in sync, and it survives a restart because both halves come off disk.
function geoPendingDecisions() {
  return (state.geoCoverage?.worklist || []).filter(entry => geoDecisionFor(entry.location) !== undefined);
}

async function applyGeoDecision(location, action, country) {
  const root = state.library?.root || state.settings?.lastRoot;
  if (!root || state.geoOverrideBusy) return;
  state.geoOverrideBusy = location;
  renderGeoWorklist();
  try {
    state.geoOverrides = await window.categorizerAPI.setGeoOverride(root, location, action, country);
  } catch (error) {
    showToast(errorText(error));
  } finally {
    state.geoOverrideBusy = null;
  }
  renderGeoWorklist();
}

function renderGeoWorklist() {
  els.geoWorklist.innerHTML = '';
  const worklist = state.geoCoverage?.worklist || [];
  if (!worklist.length) return;

  const head = document.createElement('div');
  head.className = 'geo-section-head';
  const title = document.createElement('h2');
  title.textContent = 'Gazetteer worklist';
  const hint = document.createElement('p');
  hint.className = 'geo-section-hint';
  hint.textContent =
    'Location strings the resolver could not place, biggest payoff first — one decision here fixes ' +
    'every image that mentions the string. Name the country (or “A, B” for a border crossing), or ' +
    'reject it as non-geographic.';
  head.append(title, hint);

  // Deciding writes the gazetteer; only a derive pushes it into the records. Saying so — with the
  // button right here — is the difference between a decision made and a decision applied.
  const pending = geoPendingDecisions();
  if (pending.length) {
    const bar = document.createElement('div');
    bar.className = 'geo-worklist-pending';
    const label = document.createElement('span');
    const images = pending.reduce((total, entry) => total + entry.images, 0);
    label.textContent =
      `${pending.length} decision${pending.length === 1 ? '' : 's'} saved, ` +
      `covering ${countLabel(images)} image${images === 1 ? '' : 's'} — re-derive to apply.`;
    const deriveButton = document.createElement('button');
    deriveButton.type = 'button';
    deriveButton.className = 'button compact';
    deriveButton.textContent = 'Re-derive Now';
    deriveButton.disabled = geoPipelineBusy() || state.analyzing;
    deriveButton.addEventListener('click', () => void runGeoPipeline(['derive']));
    bar.append(label, deriveButton);
    head.append(bar);
  }

  const list = document.createElement('div');
  list.className = 'geo-worklist-items';
  for (const entry of worklist) {
    list.append(buildWorklistRow(entry));
  }

  els.geoWorklist.append(head, list);
}

// How many frames one string gets to show. Enough to see whether they agree with each other, few
// enough that opening a row is not a scroll of its own.
const WORKLIST_PREVIEW_LIMIT = 24;

async function toggleWorklistImages(location) {
  if (state.geoWorklistOpen.has(location)) {
    state.geoWorklistOpen.delete(location);
    renderGeoWorklist();
    return;
  }
  state.geoWorklistOpen.add(location);
  renderGeoWorklist();
  if (state.geoWorklistImages.has(location)) return;

  const root = state.library?.root || state.settings?.lastRoot;
  if (!root) return;
  try {
    const found = await window.categorizerAPI.getGeoLocationImages(root, location, WORKLIST_PREVIEW_LIMIT);
    // Hashes are resolved against the library already in memory rather than asking the backend to
    // re-scan 33k files for a preview strip.
    const byHash = new Map((state.library?.images || []).map(image => [image.hash, image]));
    state.geoWorklistImages.set(location, {
      total: found.total,
      // A description with no file behind it any more is not evidence of anything.
      items: found.images
        .map(item => ({ image: byHash.get(item.hash), description: item.description }))
        .filter(item => item.image),
    });
  } catch (error) {
    state.geoWorklistImages.set(location, { total: 0, items: [], error: errorText(error) });
  }
  if (state.geoWorklistOpen.has(location)) renderGeoWorklist();
}

// The evidence strip: the frames that produced the string, each opening full-size on click, each
// carrying its own description as a tooltip. This is the whole point of a worklist row — "apache
// canyon" is unanswerable as text and obvious as pictures.
function buildWorklistImages(entry) {
  const strip = document.createElement('div');
  strip.className = 'geo-worklist-shots';
  const found = state.geoWorklistImages.get(entry.location);

  if (!found) {
    strip.textContent = 'Loading images…';
    strip.classList.add('loading');
    return strip;
  }
  if (found.error) {
    strip.textContent = found.error;
    strip.classList.add('loading');
    return strip;
  }
  if (!found.items.length) {
    strip.textContent = 'No described frames carry this string any more — re-derive to refresh the worklist.';
    strip.classList.add('loading');
    return strip;
  }

  for (const { image, description } of found.items) {
    const shot = document.createElement('button');
    shot.type = 'button';
    shot.className = 'geo-worklist-shot';
    shot.title = `${image.name}\n\n${description.trim()}`;
    shot.addEventListener('click', () => openImage(image.path));
    const img = document.createElement('img');
    img.loading = 'lazy';
    img.alt = '';
    img.src = window.categorizerAPI.getFileUrl(image.thumbnailPath || image.path);
    shot.append(img);
    strip.append(shot);
  }

  if (found.total > found.items.length) {
    const more = document.createElement('span');
    more.className = 'geo-worklist-more';
    more.textContent = `+${found.total - found.items.length} more`;
    strip.append(more);
  }
  return strip;
}

function buildWorklistRow(entry) {
  // The row and its evidence strip share one wrapper so the strip can span the full width instead
  // of having to fit the row's control columns.
  const wrapper = document.createElement('div');
  wrapper.className = 'geo-worklist-entry';
  const row = document.createElement('div');
  row.className = 'geo-worklist-item';
  wrapper.append(row);
  const busy = state.geoOverrideBusy === entry.location;
  const decision = geoDecisionFor(entry.location);
  const open = state.geoWorklistOpen.has(entry.location);
  row.classList.toggle('decided', decision !== undefined);

  const count = document.createElement('button');
  count.type = 'button';
  count.className = 'geo-worklist-count';
  count.textContent = countLabel(entry.images);
  // The count IS the way in: it already says how much evidence there is, so it is the thing to
  // press to see it. A separate "view images" button would just repeat the number.
  count.title = `${entry.images.toLocaleString()} images use this location string — click to see them`;
  count.setAttribute('aria-expanded', String(open));
  count.addEventListener('click', () => toggleWorklistImages(entry.location));
  count.classList.toggle('open', open);

  const label = document.createElement('span');
  label.className = 'geo-worklist-label';
  label.textContent = entry.location;
  label.title = entry.location;
  row.append(count, label);

  if (open) wrapper.append(buildWorklistImages(entry));

  if (decision !== undefined) {
    const verdict = document.createElement('span');
    verdict.className = `geo-worklist-verdict ${decision === null ? 'rejected' : 'placed'}`;
    verdict.textContent = decision === null ? 'not a place' : decision;
    const undo = document.createElement('button');
    undo.type = 'button';
    undo.className = 'button compact ghost';
    undo.textContent = 'Undo';
    undo.disabled = busy;
    // Deleting the line is the undo — the same property the file has by hand.
    undo.addEventListener('click', () => applyGeoDecision(entry.location, 'clear'));
    row.append(verdict, undo);
    return wrapper;
  }

  const input = document.createElement('input');
  input.type = 'text';
  input.className = 'geo-worklist-input';
  input.placeholder = 'Country';
  input.disabled = busy;
  // Suggestions, not a whitelist: a country outside the 109-name reference list is legitimate and
  // the coverage view already reports those separately as `off-reference`.
  input.setAttribute('list', 'geo-country-options');
  input.addEventListener('keydown', event => {
    if (event.key !== 'Enter') return;
    event.preventDefault();
    if (input.value.trim()) applyGeoDecision(entry.location, 'place', input.value);
  });

  const save = document.createElement('button');
  save.type = 'button';
  save.className = 'button compact';
  save.textContent = 'Place';
  save.disabled = busy;
  save.addEventListener('click', () => {
    if (!input.value.trim()) {
      input.focus();
      return;
    }
    applyGeoDecision(entry.location, 'place', input.value);
  });

  const reject = document.createElement('button');
  reject.type = 'button';
  reject.className = 'button compact secondary';
  reject.textContent = 'Not a place';
  reject.title = 'Record this string as non-geographic, so it stops returning to the worklist';
  reject.disabled = busy;
  reject.addEventListener('click', () => applyGeoDecision(entry.location, 'reject'));

  row.append(input, save, reject);
  return wrapper;
}

// Country suggestions for the worklist inputs, taken from the coverage clusters so the spellings
// offered are exactly the ones the resolver and the coverage scoreboard already agree on — typing
// "USA" into an override would place images under a country the reference list has never heard of.
function renderGeoCountryOptions() {
  const names = (state.geoCoverage?.clusters || []).flatMap(cluster =>
    (cluster.countries || []).map(country => country.name));
  if (els.geoCountryOptions.childElementCount === names.length) return;
  els.geoCountryOptions.replaceChildren(...names.sort().map(name => new Option(name)));
}

function geoTimeLabel(iso) {
  if (!iso) return null;
  const ms = Date.parse(iso);
  return Number.isNaN(ms) ? null : formatDate(ms);
}

// Three dates rather than one: when the coverage was derived, when the sets beside it were built,
// and when this window last looked at the disk. The third is there because "did pressing that
// actually re-read anything" had no answer on screen before — which is most of the complaint.
function geoStampLine() {
  const parts = [];
  const derived = geoTimeLabel(state.geoCoverage?.generatedAt);
  if (derived) parts.push(`Derived ${derived}`);
  const built = geoTimeLabel(state.geoStatus?.setsBuiltAt);
  if (built) parts.push(`sets built ${built}`);
  if (state.geoCheckedAt) parts.push(`checked ${new Date(state.geoCheckedAt).toLocaleTimeString()}`);
  return parts.join(' · ');
}

// Which half of the panel has been overtaken, and by what. Hidden entirely when the sidecars agree:
// a banner that is always there is a banner nobody reads.
function renderGeoFreshness() {
  const status = state.geoStatus;
  const reasons = status?.reasons || [];
  els.geoFreshness.innerHTML = '';
  els.geoFreshness.classList.toggle('hidden', !reasons.length);
  if (!reasons.length) return;

  const head = document.createElement('div');
  head.className = 'geo-freshness-head';
  head.textContent = status.recordsStale && status.setsStale
    ? 'Coverage and country sets are both out of date'
    : status.recordsStale
      ? 'Coverage is out of date'
      : 'The country sets are out of date';

  const list = document.createElement('ul');
  list.className = 'geo-freshness-reasons';
  for (const reason of reasons) {
    const item = document.createElement('li');
    item.textContent = reason;
    list.append(item);
  }
  els.geoFreshness.append(head, list);
}

// One chip per chosen part, left to right in execution order. A finished chip keeps its result, so
// the strip doubles as the record of what the last run actually did.
function renderGeoPipeline() {
  const steps = state.geoPipeline.steps;
  els.geoPipeline.innerHTML = '';
  els.geoPipeline.classList.toggle('hidden', !steps.length);
  if (!steps.length) return;

  const marks = { pending: '·', running: '▸', done: '✓', failed: '!', skipped: '–', stopped: '■' };
  steps.forEach((step, index) => {
    const chip = document.createElement('div');
    chip.className = `geo-pipeline-step ${step.state}`;

    const mark = document.createElement('span');
    mark.className = 'geo-pipeline-mark';
    mark.textContent = marks[step.state] || '·';

    const label = document.createElement('span');
    label.textContent = `${index + 1}. ${step.label}`;

    chip.append(mark, label);
    if (step.detail) {
      const detail = document.createElement('span');
      detail.className = 'geo-pipeline-detail';
      detail.textContent = step.detail;
      chip.append(detail);
    }
    chip.setAttribute('aria-label', `Step ${index + 1} ${step.label}: ${step.state}. ${step.detail}`);
    els.geoPipeline.append(chip);
  });
}

function renderGeo() {
  const busy = geoPipelineBusy() || state.analyzing;
  const chosen = GEO_PIPELINE_STEPS.filter(step => els[step.checkbox].checked).length;
  els.geoRunButton.disabled = busy || !chosen;
  els.geoRunButton.textContent = state.geoPipeline.active
    ? 'Running…'
    : chosen === GEO_PIPELINE_STEPS.length
      ? 'Run all'
      : `Run ${chosen}`;
  els.geoStopButton.classList.toggle('hidden', !state.geoPipeline.active);
  els.geoStopButton.disabled = state.geoPipeline.cancelled;
  // The pending count is the reason to tick Classify at all, so it belongs on the tick box.
  const pending = state.kindSummary?.pending;
  els.geoStepClassify.nextElementSibling.textContent =
    pending ? `Classify Scenes (${pending} left)` : 'Classify Scenes';
  for (const step of GEO_PIPELINE_STEPS) els[step.checkbox].disabled = busy;
  renderGeoPipeline();
  els.geoRefreshButton.disabled = busy || state.geoRefreshing;
  els.geoRefreshButton.textContent = state.geoRefreshing ? 'Refreshing…' : 'Refresh';
  els.geoGenerated.textContent = geoStampLine();
  renderGeoFreshness();
  renderGeoCountryOptions();
  renderGeoStats();
  renderGeoKinds();
  renderGeoLegend();
  renderGeoClusters();
  renderGeoSets();
  renderGeoReview();
  renderGeoWorklist();
}

// Patches the one image that changed rather than reloading the library. The sidebar's tallies come
// from `categoryCountsForIncludedSources()`, which recounts `state.library.images` on every render,
// so updating the record in place is all that's needed to keep every count honest.
async function assignCategory(hash, category) {
  if (!state.library) return;
  // Records are keyed by hash but the grid has one card per FILE, so duplicate files share a single
  // record. Patch every card carrying this hash — the one save governs all of them, and patching
  // only the first would leave its twins displaying a category the backend no longer agrees with.
  const images = (state.library.images || []).filter(item => item.hash === hash);
  try {
    const result = await window.categorizerAPI.assignCategory(state.library.root, hash, category);
    for (const image of images) {
      image.category = category || null;
      image.classifiedBy = result?.classifiedBy ?? null;
      image.classifiedAt = result?.classifiedAt ?? null;
    }
    render();
    showToast(category ? `Assigned to ${category}` : 'Marked unclassified');
  } catch (error) {
    showToast(errorText(error));
  }
}

// Every open goes through the shell, which starts a whole viewer process per
// call — so a double-click on a thumbnail fired two of them ~40 ms apart, and
// the second one is pure waste however the viewer handles it. Collapse repeats
// of the same file within roughly the OS double-click threshold: one gesture,
// one window. One slot is enough, since a double-click is the same path twice.
const OPEN_IMAGE_REPEAT_MS = 750;
let lastImageOpen = { path: '', at: 0 };

async function openImage(filePath) {
  const now = Date.now();
  if (filePath === lastImageOpen.path && now - lastImageOpen.at < OPEN_IMAGE_REPEAT_MS) return;
  lastImageOpen = { path: filePath, at: now };
  try {
    await window.categorizerAPI.openImage(filePath);
  } catch (error) {
    showToast(errorText(error));
  }
}

async function revealImage(filePath) {
  try {
    await window.categorizerAPI.revealImage(filePath);
  } catch (error) {
    showToast(errorText(error));
  }
}

// ==============================
// Dialogs
//
// Every dialog is opened NON-modally and dimmed by `#dialog-scrim` rather than the browser's own
// ::backdrop. `showModal()` marks the whole document outside the dialog inert, and since the window
// is undecorated that now includes the title bar — with Settings open, the window's close, minimize
// and maximize buttons all stopped responding. The scrim reproduces the modality over the part of
// the window where it belongs, stops where the frame begins, and dismisses on a click.
//
// Each dialog closes through its own `close…` function because most of them clear pending state
// (the paths queued for import, the image a move is about to act on) that a bare `.close()` leaves
// behind, so every dismissal path — Done, Cancel, Escape, clicking away — must go through them.
// ==============================

function dialogClosers() {
  return [
    [els.categoryDialog, closeCategoryDialog],
    [els.categoryRenameDialog, closeCategoryRenameDialog],
    [els.moveDialog, closeMoveDialog],
    [els.importDialog, closeImportDialog],
    [els.settingsDialog, closeSettingsDialog],
  ];
}

function openDialog(dialog) {
  dialog.show();
  syncDialogScrim();
}

function syncDialogScrim() {
  document.body.classList.toggle('dialog-open', dialogClosers().some(([dialog]) => dialog.open));
}

function dismissOpenDialogs() {
  for (const [dialog, close] of dialogClosers()) {
    if (dialog.open) close();
  }
}

function installDialogDismissal() {
  // A dialog also closes without passing through `dismissOpenDialogs` — `<form method="dialog">`
  // does it natively — so the scrim follows the `close` event, which every path fires.
  for (const [dialog] of dialogClosers()) {
    dialog.addEventListener('close', syncDialogScrim);
  }
  // `click` rather than `mousedown`: a drag that starts on a slider inside the dialog and releases
  // out here would otherwise dismiss it mid-gesture.
  els.dialogScrim.addEventListener('click', event => {
    if (event.target === els.dialogScrim) dismissOpenDialogs();
  });
}

function openCategoryDialog() {
  els.categoryNameInput.value = '';
  openDialog(els.categoryDialog);
  setTimeout(() => els.categoryNameInput.focus(), 0);
}

function closeCategoryDialog() {
  els.categoryDialog.close();
}

function openCategoryRenameDialog(name) {
  state.pendingCategoryRenameName = name;
  els.categoryRenameInput.value = name;
  openDialog(els.categoryRenameDialog);
  setTimeout(() => {
    els.categoryRenameInput.focus();
    els.categoryRenameInput.select();
  }, 0);
}

function closeCategoryRenameDialog() {
  state.pendingCategoryRenameName = null;
  els.categoryRenameDialog.close();
}

function openMoveDialog(image) {
  state.pendingMoveHash = image.hash;
  // Duplicates share a hash, so remember the exact file this card stands for.
  state.pendingMoveRelativePath = image.relativePath;
  const folders = (state.library?.sourceFolders || []).filter(folder => folder.name !== 'Root');
  els.moveFolderSelect.replaceChildren(
    ...folders.map(folder => new Option(folder.name, folder.name, false, folder.name === image.sourceFolder))
  );
  els.moveNewFolderInput.value = '';
  openDialog(els.moveDialog);
}

function closeMoveDialog() {
  state.pendingMoveHash = null;
  state.pendingMoveRelativePath = null;
  els.moveDialog.close();
}

async function submitMove() {
  if (!state.pendingMoveHash || !state.library) return;
  const targetFolder = els.moveNewFolderInput.value.trim() || els.moveFolderSelect.value;
  if (!targetFolder) return;
  const relativePath = state.pendingMoveRelativePath;
  try {
    state.library = await window.categorizerAPI.moveImage(
      state.library.root, state.pendingMoveHash, relativePath, targetFolder);
    closeMoveDialog();
    render();
    showToast(`Moved to ${targetFolder}`);
  } catch (error) {
    showToast(errorText(error));
  }
}

// Imports land in a month-stamped folder by default, so a drop needs no decision to go somewhere
// sensible — the dialog still lets you redirect it before it happens.
function defaultImportFolder() {
  const now = new Date();
  return `Imported ${String(now.getMonth() + 1).padStart(2, '0')}-${now.getFullYear()}`;
}

function openImportDialog(paths) {
  if (!state.library) {
    showToast('Choose a root folder first.');
    return;
  }
  // The buttons are disabled while analyzing, but a drag-drop reaches this directly — so the guard
  // has to live here, at the one point every import path goes through.
  if (state.analyzing) {
    showToast('Analysis is running — wait for it to finish before importing.');
    return;
  }
  state.pendingImportPaths = paths;
  const folders = (state.library.sourceFolders || []).filter(folder => folder.name !== 'Root');
  const suggested = defaultImportFolder();
  els.importCount.textContent =
    paths.length === 1 ? '1 item selected' : `${paths.length} items selected`;
  els.importFolderSelect.replaceChildren(...folders.map(folder => new Option(folder.name, folder.name)));
  els.importNewFolderInput.value = folders.some(folder => folder.name === suggested) ? '' : suggested;
  openDialog(els.importDialog);
  setTimeout(() => els.importNewFolderInput.focus(), 0);
}

function closeImportDialog() {
  state.pendingImportPaths = null;
  els.importDialog.close();
}

async function submitImport() {
  const paths = state.pendingImportPaths;
  if (!paths || !state.library) return;
  const targetFolder = els.importNewFolderInput.value.trim() || els.importFolderSelect.value;
  if (!targetFolder) {
    showToast('Pick or name a folder to import into.');
    return;
  }
  const root = state.library.root;
  closeImportDialog();
  setStatus(`Importing ${paths.length} item${paths.length === 1 ? '' : 's'} into ${targetFolder}…`);
  try {
    const report = await window.categorizerAPI.importImages(root, targetFolder, paths);
    await refreshAll();
    const parts = [`Imported ${report.imported} image${report.imported === 1 ? '' : 's'} into ${report.targetFolder}`];
    if (report.skipped) parts.push(`${report.skipped} skipped`);
    showToast(parts.join(' — '));
    if (report.errors?.length) console.warn('Import errors:', report.errors);
  } catch (error) {
    showToast(errorText(error));
    setStatus('Import failed.');
  }
}

async function importFromPicker(chooser) {
  if (!state.library) {
    showToast('Choose a root folder first.');
    return;
  }
  try {
    const paths = await chooser();
    if (paths?.length) openImportDialog(paths);
  } catch (error) {
    showToast(errorText(error));
  }
}

function openSettingsDialog() {
  syncSettingsDialog();
  syncNsfwModelHint();
  loadVisionAndChunkSettings();
  // Reads the live window rect, so it has to be re-fetched on every open rather than cached.
  loadWindowDefaults();
  openDialog(els.settingsDialog);
}

function closeSettingsDialog() {
  // Tile size and dark mode are applied live but persisted on the way out, so the save belongs to
  // every way of leaving — Done, Escape, and clicking away — not just to the submit button.
  saveUiSettingsNow();
  els.settingsDialog.close();
}

async function createCategory(name) {
  if (!state.library) {
    showToast('Choose a root folder first.');
    return;
  }
  try {
    state.library = await window.categorizerAPI.createCategory(state.library.root, name);
    closeCategoryDialog();
    render();
    showToast(`Created ${name.trim()}`);
  } catch (error) {
    showToast(errorText(error));
  }
}

async function renamePendingCategory(newName) {
  if (!state.pendingCategoryRenameName || !state.library) return;
  const oldName = state.pendingCategoryRenameName;
  try {
    state.library = await window.categorizerAPI.renameCategory(state.library.root, oldName, newName);
    const wasCurrent = state.currentCategory === oldName;
    closeCategoryRenameDialog();
    if (wasCurrent) state.currentCategory = newName.trim();
    // A history entry names its category, so a rename has to follow it into the trail — otherwise
    // Back lands on a name the library no longer has and the entry gets dropped.
    for (const entry of state.nav.entries) {
      if (entry.view === 'category' && entry.category === oldName) entry.category = newName.trim();
    }
    render();
    showToast(`Renamed to ${newName.trim()}`);
  } catch (error) {
    showToast(errorText(error));
  }
}

async function deleteCategoryConfirm(name) {
  if (!state.library) return;
  if (!window.confirm(`Delete category "${name}"? Images in it become unclassified.`)) return;
  try {
    state.library = await window.categorizerAPI.deleteCategory(state.library.root, name);
    const wasCurrent = state.currentCategory === name;
    if (wasCurrent) {
      state.currentView = 'all';
      state.currentCategory = null;
    }
    pruneNavEntries(entry => !(entry.view === 'category' && entry.category === name));
    if (wasCurrent) pushNavEntry(navEntry('all'));
    render();
    showToast(`Deleted ${name}`);
  } catch (error) {
    showToast(errorText(error));
  }
}

async function removeManualSourceFolder(name) {
  if (!state.library) return;
  try {
    state.library = await window.categorizerAPI.removeManualSourceFolder(state.library.root, name);
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

async function addManualSourceFolder() {
  if (!state.library) {
    showToast('Choose a root folder first.');
    return;
  }
  try {
    const library = await window.categorizerAPI.chooseManualSourceFolder(state.library.root);
    if (!library) return;
    state.library = library;
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

async function saveSourcePattern() {
  if (!state.library) return;
  const preset = els.sourcePatternPreset.value || null;
  const regex = els.sourcePatternRegex.value.trim() || null;
  try {
    state.library = await window.categorizerAPI.setSourcePattern(state.library.root, preset, regex);
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

function syncTextThresholdLabels() {
  els.ocrWordThresholdValue.textContent = `${els.ocrWordThresholdInput.value} words`;
  els.ocrAreaThresholdValue.textContent = `${els.ocrAreaThresholdInput.value}%`;
}

function syncNsfwThresholdLabel() {
  els.nsfwThresholdValue.textContent = `${els.nsfwThresholdInput.value}%`;
}

async function saveTextThresholds() {
  if (!state.library) return;
  const wordThreshold = Number(els.ocrWordThresholdInput.value);
  const areaThreshold = Number(els.ocrAreaThresholdInput.value) / 100;
  try {
    state.library = await window.categorizerAPI.setTextThresholds(state.library.root, wordThreshold, areaThreshold);
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

async function saveNsfwThreshold() {
  if (!state.library) return;
  const threshold = Number(els.nsfwThresholdInput.value) / 100;
  try {
    state.library = await window.categorizerAPI.setNsfwThreshold(state.library.root, threshold);
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

async function setFolderAnalysisIncluded(folderName, included) {
  if (!state.library) return;
  try {
    state.library = await window.categorizerAPI.setFolderAnalysisIncluded(state.library.root, folderName, included);
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

async function setCategoryAnalysisIncluded(categoryName, included) {
  if (!state.library) return;
  try {
    state.library = await window.categorizerAPI.setCategoryAnalysisIncluded(state.library.root, categoryName, included);
    render();
    showToast(included ? `“${categoryName}” included in analysis` : `“${categoryName}” omitted from analysis`);
  } catch (error) {
    showToast(errorText(error));
  }
}

// ==============================
// Unified analysis queue
// ==============================

function setInteractionsLocked(locked) {
  state.analyzing = locked;
  updateActivityIndicator();
  els.addCategoryButton.disabled = locked;
  els.addSourceFolderButton.disabled = locked;
  els.rootFolderSelect.disabled = locked;
  els.refreshButton.disabled = locked;
  els.openFolderButton.disabled = locked;
  els.settingsButton.disabled = locked;
  // Importing writes the sidecar too, and a running analysis pass overwrites that file wholesale
  // from a snapshot it took when it started — an import landing mid-pass would be erased by it.
  els.importButton.disabled = locked;
  els.importFolderButton.disabled = locked;
  els.analyzeButton.classList.toggle('hidden', locked);
  els.reanalyzeButton.classList.toggle('hidden', locked);
  els.cancelAnalysisButton.classList.toggle('hidden', !locked);
  render();
}

function analysisTypeLabel(type) {
  if (type === 'nsfw') return 'Explicit';
  if (type === 'ocr') return 'Extract Text';
  if (type === 'chunk') return 'Video Dedup';
  if (type === 'vision') return 'Describe';
  return 'Text';
}

async function runNextInQueue() {
  if (!state.analysisQueue.length) {
    const extractionRan = state.analysisRan?.has('ocr');
    state.analysisRan = null;
    // All done — refresh and unlock
    state.analysisRunning = null;
    setInteractionsLocked(false);
    if (state.library) {
      try {
        setLoading(true);
        setStatus('Refreshing library…', true);
        render();
        state.library = await window.categorizerAPI.scanLibrary(state.library.root);
        setLoading(false);
        render();
        setStatus('Analysis complete.');
        // New extracted text makes the search index stale by construction. Rebuilding here keeps
        // the cost inside the run that caused it, instead of charging it to the next search.
        if (extractionRan) {
          await rebuildTextIndex();
          await loadTextStatus();
        }
      } catch (error) {
        setLoading(false);
        setStatus('');
        showToast(errorText(error));
      }
    }
    return;
  }

  const { type, force, indexedOnly } = state.analysisQueue.shift();
  state.analysisRan = state.analysisRan || new Set();
  state.analysisRan.add(type);
  state.analysisRunning = type;
  const verb = force ? 'Re-analyzing' : 'Analyzing';
  setStatus(`${verb} (${analysisTypeLabel(type)})…`);

  try {
    if (type === 'text') {
      await window.categorizerAPI.analyzeText(state.library.root, force);
    } else if (type === 'ocr') {
      await window.categorizerAPI.extractText(state.library.root, force, !!indexedOnly);
    } else if (type === 'chunk') {
      await window.categorizerAPI.buildChunkPlan(state.library.root, force);
    } else if (type === 'vision') {
      await window.categorizerAPI.analyzeVision(state.library.root, force);
    } else {
      await window.categorizerAPI.analyzeNsfw(state.library.root, force);
    }
  } catch (error) {
    showToast(errorText(error));
    // Skip to next
    await runNextInQueue();
  }
}

function formatCount(value) {
  return Number(value || 0).toLocaleString();
}

// Queue order, which is also the order the breakdown reads in. The bits match `PASS_BIT_*` in
// lib.rs — the index into the mask table the scan hands over.
const ANALYSIS_TYPE_ORDER = ['nsfw', 'chunk', 'text', 'ocr', 'vision'];
const ANALYSIS_PASS_BIT = { nsfw: 1, chunk: 2, text: 4, ocr: 8, vision: 16 };

function selectedAnalysisTypes() {
  const checks = {
    nsfw: els.analyzeNsfwCheck,
    chunk: els.analyzeChunkCheck,
    text: els.analyzeTextCheck,
    ocr: els.extractTextCheck,
    vision: els.analyzeVisionCheck,
  };
  return ANALYSIS_TYPE_ORDER.filter(type => checks[type].checked);
}

// A UNION over the ticked passes, never a sum: an image that is new to both Explicit and Text is
// one image to analyze. That is why the backend ships a table of per-combination counts — summing
// per-pass totals would double-count every image more than one pass is waiting on.
function newImageCount(types) {
  const masks = state.library?.pending?.byPassMask;
  if (!masks) return 0;
  const selection = types.reduce((bits, type) => bits | ANALYSIS_PASS_BIT[type], 0);
  return masks.reduce((sum, count, mask) => (mask & selection ? sum + count : sum), 0);
}

// Describe's number is measured against the library as it stands right now, and the passes queued
// ahead of it change what it will see by the time it runs: Explicit scores the images Describe
// refuses to look at unscored, and Video Dedup rebuilds the plan deciding which frames are sampled.
// Saying so is the difference between a number that looks wrong later and one that was honest.
function pendingCountCaveat(types) {
  const pending = state.library?.pending;
  if (!pending || !types.includes('vision')) return '';
  const notes = [];
  const unscored = pending.visionSkippedUnscored || 0;
  if (unscored > 0) {
    notes.push(types.includes('nsfw')
      ? `${formatCount(unscored)} more unlock once Explicit has scored them`
      : `${formatCount(unscored)} stay out until Explicit is run`);
  }
  if (types.includes('chunk')) notes.push('Video Dedup rebuilds the sample plan first');
  if (!notes.length) return '';
  return ` Describe's ${formatCount(newImageCount(['vision']))} is a snapshot — ${notes.join('; ')}.`;
}

function pendingBreakdown(types) {
  return types.map(type => `${analysisTypeLabel(type)} ${formatCount(newImageCount([type]))}`).join(' · ');
}

// The standing readout in the analysis row. It answers "how many are new" for whatever is ticked
// right now, off the last scan — no round trip, so ticking a box re-answers it instantly.
function renderAnalysisPending() {
  const element = els.analysisPending;
  const pending = state.library?.pending;
  if (!pending) {
    element.hidden = true;
    return;
  }
  element.hidden = false;

  const types = selectedAnalysisTypes();
  if (!types.length) {
    element.textContent = 'no type ticked';
    element.title = 'Tick an analysis type to see how many images are new to it.';
    element.classList.add('is-clear');
    return;
  }

  if (!pending.anyFolderIncluded) {
    element.textContent = 'no folders included';
    element.title = 'Every source folder is switched off for analysis, so no pass has anything to work on.';
    element.classList.add('is-clear');
    return;
  }

  const total = newImageCount(types);
  element.textContent = total ? `${formatCount(total)} new` : 'nothing new';
  element.classList.toggle('is-clear', total === 0);

  // The tooltip carries what won't fit on the chip: the per-pass split, and where Describe's
  // missing images went — "Describe 0" out of thousands usually means unscored, not done.
  const lines = [
    `${pendingBreakdown(types)}`,
    `${formatCount(pending.eligibleImages)} images eligible for analysis`,
  ];
  if (types.includes('vision')) {
    lines.push(
      `Describe skips: ${formatCount(pending.visionSkippedUnscored)} not yet Explicit-analyzed, `
      + `${formatCount(pending.visionSkippedExplicit)} explicit, `
      + `${formatCount(pending.visionSkippedVideo)} deduped video frames, `
      + `${formatCount(pending.visionSkippedCategory)} in omitted categories`,
    );
  }
  lines.push('Counted at the last scan — press Rescan to take in files added since.');
  element.title = lines.join('\n');
}

async function startAnalysis(force) {
  if (!state.library) {
    showToast('Choose a root folder first.');
    return;
  }
  if (state.analyzing) return;

  const types = selectedAnalysisTypes();
  if (!types.length) {
    showToast('Select at least one analysis type.');
    return;
  }

  // Only "Analyze New" has anything to say up front. "Re-analyze All" is by definition every
  // eligible image, which is what the button already says. `byPassMask` missing means the counts
  // never arrived, and an absent count must not be read as "nothing to do" — run and stay quiet.
  if (!force && state.library.pending?.byPassMask) {
    const total = newImageCount(types);
    const breakdown = pendingBreakdown(types);
    if (!state.library.pending?.anyFolderIncluded) {
      const message = 'No source folders are included in analysis.';
      setStatus(message);
      showToast(message, { sticky: true });
      return;
    }
    if (!total) {
      // Video Dedup regroups the frames and re-samples every video even when there is no title
      // strip left to OCR, so with it ticked the run still has real work to do.
      if (!types.includes('chunk')) {
        setStatus(`Nothing new to analyze — ${breakdown}.`);
        showToast(`Nothing new to analyze: ${breakdown}.${pendingCountCaveat(types)}`, { sticky: true });
        return;
      }
      setStatus('No new images — running Video Dedup to rebuild the video plan.');
    } else {
      const headline = `${formatCount(total)} new ${total === 1 ? 'image' : 'images'} to analyze — ${breakdown}`;
      showToast(`${headline}.${pendingCountCaveat(types)}`, { sticky: true });
    }
  }

  // Order matters: Explicit first (Describe skips explicit images), then Video Dedup (builds the
  // sample plan Describe reads), then the text passes, and Describe last so it sees the results.
  // `types` is already in that order.
  state.analysisQueue = types.map(type => ({ type, force }));

  setInteractionsLocked(true);
  await runNextInQueue();
}

async function cancelCurrentAnalysis() {
  // Cancel the running one; the queue will be drained when the finished event fires
  state.analysisQueue = [];
  try {
    if (state.analysisRunning === 'text') {
      await window.categorizerAPI.cancelTextAnalysis();
    } else if (state.analysisRunning === 'nsfw') {
      await window.categorizerAPI.cancelNsfwAnalysis();
    } else if (state.analysisRunning === 'ocr') {
      await window.categorizerAPI.cancelTextExtraction();
    } else if (state.analysisRunning === 'chunk') {
      await window.categorizerAPI.cancelChunkScan();
    } else if (state.analysisRunning === 'vision') {
      await window.categorizerAPI.cancelVisionAnalysis();
    }
    setStatus('Cancelling…');
  } catch (error) {
    showToast(errorText(error));
  }
}

async function onAnalysisFinished(type, { status, message }) {
  if (state.analysisRunning !== type) return; // stale event

  if (status === 'error') {
    state.analysisQueue = [];
    state.analysisRunning = null;
    setInteractionsLocked(false);
    setStatus('');
    showToast(message || `${analysisTypeLabel(type)} analysis failed`, { sticky: true });
    // Still refresh so partial results are visible
    if (state.library) {
      try {
        state.library = await window.categorizerAPI.scanLibrary(state.library.root);
        render();
      } catch { /* ignore */ }
    }
    return;
  }

  if (status === 'cancelled') {
    state.analysisQueue = [];
    state.analysisRunning = null;
    setInteractionsLocked(false);
    const summary = `${analysisTypeLabel(type)} analysis cancelled.`;
    showToast(summary);
    if (state.library) {
      try {
        state.library = await window.categorizerAPI.scanLibrary(state.library.root);
        render();
        setStatus(summary);
      } catch { setStatus(''); }
    } else {
      setStatus(summary);
    }
    return;
  }

  if (message) showToast(message);
  // Move to next item in queue
  await runNextInQueue();
}

async function saveUiSettingsNow() {
  if (!state.settings) return;
  const tileSizeValue = Number(els.tileSizeInput.value);
  const darkMode = els.darkModeInput.checked;
  try {
    state.settings = await window.categorizerAPI.setTileSize(tileSizeValue);
    state.settings = await window.categorizerAPI.setDarkMode(darkMode);
    renderSettings();
  } catch (error) {
    showToast(errorText(error));
  }
}

function applyPendingUiSettings() {
  if (!state.settings) return;
  state.settings.tileSize = Number(els.tileSizeInput.value);
  state.settings.darkMode = els.darkModeInput.checked;
  applyUiSettings();
  state.cardHeight = null;
  renderImages();
}

async function changeRootFolder() {
  setStatus('Loading folder…');
  try {
    const library = await window.categorizerAPI.chooseRootFolder(state.library?.root);
    if (!library) {
      setStatus('');
      return;
    }
    state.library = library;
    state.settings = await window.categorizerAPI.getSettings();
    state.currentView = 'all';
    state.currentCategory = null;
    resetNavHistory();
    render();
    setStatus(`Loaded ${imageCountLabel(state.library.images.length)}.`);
  } catch (error) {
    setStatus('');
    showToast(errorText(error));
  }
}

async function selectRootFolder(rootPath) {
  if (!rootPath || rootPath === state.library?.root) return;
  setStatus('Loading folder…');
  try {
    state.library = await window.categorizerAPI.selectRootFolder(rootPath);
    state.settings = await window.categorizerAPI.getSettings();
    state.currentView = 'all';
    state.currentCategory = null;
    resetNavHistory();
    render();
    setStatus(`Loaded ${imageCountLabel(state.library.images.length)}.`);
  } catch (error) {
    setStatus('');
    showToast(errorText(error));
    renderRootFolderSelect();
  }
}

async function openCurrentRootFolder() {
  if (!state.library?.root) return;
  try {
    await window.categorizerAPI.openRootFolder(state.library.root);
  } catch (error) {
    showToast(errorText(error));
  }
}

function clearPointerDropTargets() {
  document.querySelectorAll('.pointer-drop-over').forEach(element => element.classList.remove('pointer-drop-over'));
}

function categoryDropTargetFromPoint(x, y) {
  const element = document.elementFromPoint(x, y);
  const categoryRow = element?.closest?.('.category-row');
  if (categoryRow?.dataset.categoryName) {
    return { element: categoryRow, category: categoryRow.dataset.categoryName };
  }
  if (element?.closest?.('#unclassified-tab')) {
    return { element: els.unclassifiedTab, category: null };
  }
  return null;
}

// Horizontal panning for the overflow a narrow window produces. Worth wiring by hand because the
// things that overflow first are the topbar's action buttons — exactly what you need when the
// window is small — and WebView2 gives no middle-click autoscroll of its own. Middle-drag to pan,
// Shift+wheel as the conventional equivalent. Both no-op when there is nothing to scroll.
function installHorizontalPan() {
  const surface = els.main;
  if (!surface) return;
  let pan = null;

  const canScroll = () => surface.scrollWidth > surface.clientWidth;

  surface.addEventListener('pointerdown', event => {
    if (event.button !== 1 || !canScroll()) return;
    event.preventDefault();
    pan = { startX: event.clientX, startLeft: surface.scrollLeft, pointerId: event.pointerId };
    // Capture keeps the pan alive when the cursor crosses a child or leaves the window mid-drag.
    // It throws if the pointer is no longer active, which must not abort the pan itself.
    try {
      surface.setPointerCapture(event.pointerId);
    } catch {}
    surface.classList.add('panning');
  });

  surface.addEventListener('pointermove', event => {
    if (!pan || event.pointerId !== pan.pointerId) return;
    surface.scrollLeft = pan.startLeft - (event.clientX - pan.startX);
  });

  const endPan = event => {
    if (!pan || (event && event.pointerId !== pan.pointerId)) return;
    if (surface.hasPointerCapture?.(pan.pointerId)) surface.releasePointerCapture(pan.pointerId);
    pan = null;
    surface.classList.remove('panning');
  };
  surface.addEventListener('pointerup', endPan);
  surface.addEventListener('pointercancel', endPan);

  // Without this, releasing the middle button still fires the click that would paste-on-Linux /
  // open the autoscroll widget, and it can land on whatever button was underneath the drag.
  surface.addEventListener('auxclick', event => {
    if (event.button === 1) event.preventDefault();
  });

  surface.addEventListener(
    'wheel',
    event => {
      if (!event.shiftKey || !canScroll()) return;
      event.preventDefault();
      surface.scrollLeft += event.deltaY || event.deltaX;
    },
    { passive: false }
  );
}

// A narrow window stacks the sidebar above the grid, so it spends the grid's vertical space rather
// than its horizontal space — which makes it worth being able to put away while browsing. Stored in
// localStorage rather than the app settings file: it's a per-window-shape view preference, not
// library state, and it needs no round trip to Rust. The stylesheet honours the class only in the
// stacked layout, so widening the window always brings the sidebar back whether or not it's set.
const SIDEBAR_COLLAPSED_KEY = 'imageCategorizer.sidebarCollapsed';

function applySidebarCollapsed(collapsed) {
  document.body.classList.toggle('sidebar-collapsed', collapsed);
  els.sidebarToggle.setAttribute('aria-expanded', String(!collapsed));
  els.sidebarToggle.title = collapsed ? 'Show the sidebar' : 'Hide the sidebar';
  // Back/Forward live in the sidebar head, which the narrow layout hides entirely — so the group
  // is MOVED rather than duplicated. One node keeps one set of ids, so nothing else has to know
  // which layout is in force. It lands after the toggle that produced the collapse.
  if (collapsed) els.sidebarToggle.after(els.navButtons);
  else els.brand.prepend(els.navButtons);
}

function installSidebarToggle() {
  let collapsed = false;
  // Private-mode/partitioned webviews can throw on either access; the toggle still works, it just
  // won't be remembered.
  try {
    collapsed = localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === '1';
  } catch {}
  applySidebarCollapsed(collapsed);

  els.sidebarToggle.addEventListener('click', () => {
    const next = !document.body.classList.contains('sidebar-collapsed');
    applySidebarCollapsed(next);
    try {
      localStorage.setItem(SIDEBAR_COLLAPSED_KEY, next ? '1' : '0');
    } catch {}
    // The grid just gained or lost the sidebar's height, so its column count and the virtual
    // window's row maths both need recomputing — same work the resize handler does.
    state.cardHeight = null;
    renderImages();
  });
}

// ==============================
// Custom title bar
// The window is undecorated, so the strip at the top of the page IS the frame: Tauri's own
// drag-region script handles moving and double-click-to-maximize, and everything below supplies the
// parts it doesn't — the three buttons, the maximized/restored glyph, and the edge resize handles a
// borderless window has no OS border for.
// ==============================

async function syncMaximizedState() {
  try {
    const maximized = await window.categorizerAPI.isWindowMaximized();
    document.body.classList.toggle('window-maximized', !!maximized);
    els.titlebarMaximize.title = maximized ? 'Restore down' : 'Maximize';
    els.titlebarMaximize.setAttribute('aria-label', maximized ? 'Restore down' : 'Maximize');
  } catch {}
}

async function installTitlebar() {
  els.titlebarMinimize.addEventListener('click', () => {
    window.categorizerAPI.minimizeWindow?.();
  });
  els.titlebarMaximize.addEventListener('click', async () => {
    await window.categorizerAPI.toggleMaximizeWindow?.();
    syncMaximizedState();
  });
  els.titlebarClose.addEventListener('click', () => {
    window.categorizerAPI.closeWindow?.();
  });

  for (const grip of els.resizeGrips.querySelectorAll('.resize-grip')) {
    grip.addEventListener('mousedown', event => {
      if (event.button !== 0) return;
      // Without this, an ancestor turning the press into startDragging would move the window
      // instead of resizing it.
      event.preventDefault();
      event.stopPropagation();
      window.categorizerAPI.startResizeDragging?.(grip.dataset.resizeDir);
    });
  }

  // Aero Snap and the drag-region's own double-click both maximize without going through the
  // button, so the glyph has to follow the window rather than the click that was made here.
  try {
    await window.categorizerAPI.onWindowResized?.(syncMaximizedState);
  } catch {}
  syncMaximizedState();
}

// ==============================
// Saved window position and size
// ==============================

function windowBoundsLabel(bounds) {
  if (!bounds) return 'unknown';
  return `${bounds.width} × ${bounds.height} at ${bounds.x}, ${bounds.y}`;
}

function renderWindowDefaults(defaults) {
  if (!defaults) {
    els.windowDefaultsStatus.textContent = '';
    return;
  }
  // The saved rect is kept alongside the maximized flag — it's the size the window comes back to
  // when it's restored down, which is worth saying out loud since nothing on screen shows it.
  const saved = defaults.savedMaximized
    ? `maximized${defaults.saved ? ` (restoring down to ${windowBoundsLabel(defaults.saved)})` : ''}`
    : windowBoundsLabel(defaults.saved);
  const current = `Now: ${defaults.currentMaximized ? 'maximized' : windowBoundsLabel(defaults.current)}.`;
  els.windowDefaultsStatus.textContent = defaults.saved || defaults.savedMaximized
    ? `Opens at ${saved}. ${current}`
    : `No saved default — opens at its built-in size, placed by Windows. ${current}`;
  els.clearWindowDefaultsButton.disabled = !defaults.saved && !defaults.savedMaximized;
}

async function loadWindowDefaults() {
  try {
    renderWindowDefaults(await window.categorizerAPI.getWindowDefaults());
  } catch (error) {
    els.windowDefaultsStatus.textContent = errorText(error);
  }
}

async function saveWindowDefaults() {
  try {
    renderWindowDefaults(await window.categorizerAPI.saveWindowDefaults());
    showToast('Saved this position and size as the default.');
  } catch (error) {
    showToast(errorText(error));
  }
}

async function clearWindowDefaults() {
  try {
    renderWindowDefaults(await window.categorizerAPI.clearWindowDefaults());
    showToast('Cleared the saved window default.');
  } catch (error) {
    showToast(errorText(error));
  }
}

function startPointerDrag(event, card) {
  if (state.analyzing || event.button !== 0 || event.target.closest('button, select, .analysis-summary')) return;

  state.pointerDrag = {
    card,
    hash: card.dataset.hash,
    name: card.dataset.name || card.dataset.hash,
    startX: event.clientX,
    startY: event.clientY,
    x: event.clientX,
    y: event.clientY,
    active: false,
    ghost: null,
    dropTarget: null,
  };

  card.setPointerCapture?.(event.pointerId);
  card.addEventListener('pointermove', onPointerDragMove);
  card.addEventListener('pointerup', onPointerDragEnd, { once: true });
  card.addEventListener('pointercancel', cancelPointerDrag, { once: true });
}

function activatePointerDrag(drag) {
  drag.active = true;
  drag.card.classList.add('dragging');
  document.body.classList.add('pointer-dragging');

  const ghost = document.createElement('div');
  ghost.className = 'drag-ghost';
  ghost.textContent = drag.name;
  document.body.append(ghost);
  drag.ghost = ghost;
  moveDragGhost(drag);
}

function moveDragGhost(drag) {
  if (!drag.ghost) return;
  drag.ghost.style.transform = `translate(${drag.x + 12}px, ${drag.y + 12}px)`;
}

function onPointerDragMove(event) {
  const drag = state.pointerDrag;
  if (!drag) return;

  drag.x = event.clientX;
  drag.y = event.clientY;

  if (!drag.active) {
    const dx = drag.x - drag.startX;
    const dy = drag.y - drag.startY;
    if (Math.hypot(dx, dy) < 6) return;
    activatePointerDrag(drag);
  }

  event.preventDefault();
  moveDragGhost(drag);
  clearPointerDropTargets();
  drag.dropTarget = categoryDropTargetFromPoint(drag.x, drag.y);
  drag.dropTarget?.element.classList.add('pointer-drop-over');
}

function finishPointerDrag() {
  const drag = state.pointerDrag;
  if (!drag) return null;

  drag.card.removeEventListener('pointermove', onPointerDragMove);
  drag.card.classList.remove('dragging');
  document.body.classList.remove('pointer-dragging');
  clearPointerDropTargets();
  drag.ghost?.remove();
  state.pointerDrag = null;
  return drag;
}

function cancelPointerDrag() {
  finishPointerDrag();
}

function onPointerDragEnd(event) {
  const drag = finishPointerDrag();
  if (!drag?.active) return;
  event.preventDefault();
  if (drag.dropTarget) {
    assignCategory(drag.hash, drag.dropTarget.category);
  }
}

function installEvents() {
  resetNavHistory(state.currentView);
  installNavShortcuts();
  installDashboard();
  els.allTab.addEventListener('click', selectAll);
  els.unclassifiedTab.addEventListener('click', selectUnclassified);
  els.geoTab.addEventListener('click', selectGeo);
  els.textTab.addEventListener('click', selectText);
  installAutomationPanel();
  wireTextPanel();
  els.geoRunButton.addEventListener('click', () => void runSelectedGeoPipeline());
  els.geoStopButton.addEventListener('click', () => void stopGeoPipeline());
  for (const step of GEO_PIPELINE_STEPS) {
    els[step.checkbox].addEventListener('change', () => renderGeo());
  }
  els.geoRefreshButton.addEventListener('click', () => void refreshGeoData());
  // Both, because neither is reliable alone in WebView2: `focus` misses a re-show that never took
  // keyboard focus, and `visibilitychange` does not fire for a window merely raised over another.
  // `checkGeoFreshness` is cheap and idempotent, so firing twice costs nothing.
  window.addEventListener('focus', () => void checkGeoFreshness());
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) void checkGeoFreshness();
  });
  els.geoGazetteerButton.addEventListener('click', async () => {
    const root = state.library?.root || state.settings?.lastRoot;
    if (!root) return;
    try {
      await window.categorizerAPI.openGeoGazetteer(root);
    } catch (error) {
      showToast(errorText(error));
    }
  });
  els.addCategoryButton.addEventListener('click', openCategoryDialog);
  els.addSourceFolderButton.addEventListener('click', addManualSourceFolder);
  els.rootFolderSelect.addEventListener('change', () => {
    const value = els.rootFolderSelect.value;
    if (value === '__add__') {
      renderRootFolderSelect();
      changeRootFolder();
      return;
    }
    selectRootFolder(value);
  });
  els.settingsButton.addEventListener('click', openSettingsDialog);
  els.saveWindowDefaultsButton.addEventListener('click', saveWindowDefaults);
  els.clearWindowDefaultsButton.addEventListener('click', clearWindowDefaults);
  els.refreshButton.addEventListener('click', refreshAll);
  els.importButton.addEventListener('click', () =>
    importFromPicker(window.categorizerAPI.chooseImagesToImport));
  els.importFolderButton.addEventListener('click', () =>
    importFromPicker(window.categorizerAPI.chooseFolderToImport));
  els.cancelImportButton.addEventListener('click', closeImportDialog);
  els.importForm.addEventListener('submit', event => {
    event.preventDefault();
    submitImport();
  });
  els.analyzeButton.addEventListener('click', () => startAnalysis(false));
  els.reanalyzeButton.addEventListener('click', () => startAnalysis(true));
  // Ticking a pass re-answers "how many are new" from the table the last scan already handed over,
  // so the chip tracks the selection without a round trip.
  for (const check of [els.analyzeNsfwCheck, els.analyzeChunkCheck, els.analyzeTextCheck,
                       els.extractTextCheck, els.analyzeVisionCheck]) {
    check.addEventListener('change', renderAnalysisPending);
  }
  els.cancelAnalysisButton.addEventListener('click', cancelCurrentAnalysis);
  els.openFolderButton.addEventListener('click', openCurrentRootFolder);
  els.searchInput.addEventListener('input', () => {
    state.search = els.searchInput.value;
    renderImages();
  });
  els.sortSelect.addEventListener('change', () => {
    state.sort = els.sortSelect.value;
    renderImages();
  });
  els.mainDropTarget.addEventListener('scroll', onGridScroll, { passive: true });
  installHorizontalPan();
  installSidebarToggle();
  window.addEventListener('resize', () => {
    clearTimeout(installEvents.resizeTimer);
    installEvents.resizeTimer = setTimeout(() => {
      state.cardHeight = null;
      renderImages();
    }, 120);
  });

  els.categoryForm.addEventListener('submit', event => {
    event.preventDefault();
    createCategory(els.categoryNameInput.value);
  });
  els.cancelCategoryButton.addEventListener('click', closeCategoryDialog);

  els.categoryRenameForm.addEventListener('submit', event => {
    event.preventDefault();
    renamePendingCategory(els.categoryRenameInput.value);
  });
  els.cancelCategoryRenameButton.addEventListener('click', closeCategoryRenameDialog);

  els.moveForm.addEventListener('submit', event => {
    event.preventDefault();
    submitMove();
  });
  els.cancelMoveButton.addEventListener('click', closeMoveDialog);

  els.settingsForm.addEventListener('submit', event => {
    event.preventDefault();
    closeSettingsDialog();
  });
  els.settingsRootButton.addEventListener('click', changeRootFolder);
  els.sourcePatternPreset.addEventListener('change', () => {
    const preset = els.sourcePatternPreset.value;
    const presetRegexMap = {
      'YYYY-MM': '^\\d{4}-\\d{2}$',
      'YYYY_MM': '^\\d{4}_\\d{2}$',
      'MM-YYYY': '^\\d{2}-\\d{4}$',
      'Month YYYY': '^(?i)(January|February|March|April|May|June|July|August|September|October|November|December) \\d{4}$',
    };
    if (presetRegexMap[preset]) {
      els.sourcePatternRegex.value = presetRegexMap[preset];
    }
    saveSourcePattern();
  });
  els.sourcePatternRegex.addEventListener('change', saveSourcePattern);
  els.tileSizeInput.addEventListener('input', applyPendingUiSettings);
  els.darkModeInput.addEventListener('change', applyPendingUiSettings);
  els.ocrWordThresholdInput.addEventListener('input', syncTextThresholdLabels);
  els.ocrWordThresholdInput.addEventListener('change', saveTextThresholds);
  els.ocrAreaThresholdInput.addEventListener('input', syncTextThresholdLabels);
  els.ocrAreaThresholdInput.addEventListener('change', saveTextThresholds);
  els.nsfwThresholdInput.addEventListener('input', syncNsfwThresholdLabel);
  els.nsfwThresholdInput.addEventListener('change', saveNsfwThreshold);
  els.downloadNsfwModelButton.addEventListener('click', downloadNsfwModel);
  els.visionEndpointInput.addEventListener('change', saveVisionSettings);
  els.visionModelInput.addEventListener('change', saveVisionSettings);
  els.visionApiKeyInput.addEventListener('change', saveVisionSettings);
  els.visionModelSelect.addEventListener('change', () => {
    const chosen = els.visionModelSelect.value;
    if (!chosen) return;
    els.visionModelInput.value = chosen;
    saveVisionSettings();
  });
  els.refreshVisionModelsButton.addEventListener('click', refreshVisionModels);
  els.loadVisionModelButton.addEventListener('click', loadSelectedVisionModel);
  els.visionIdleUnloadInput.addEventListener('change', saveVisionSettings);
  els.visionIdleMinutesInput.addEventListener('change', saveVisionSettings);
  installAppActivityHeartbeat();
  els.toastDismiss.addEventListener('click', dismissToast);
  els.statusDismiss.addEventListener('click', clearStatus);
  els.regenerateChunkPlanButton.addEventListener('click', regenerateChunkPlan);
  els.openChunkPlanButton.addEventListener('click', openChunkPlanFile);
  els.discardChunkPlanButton.addEventListener('click', discardChunkPlan);
  els.autoRefreshEnabledInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshHour.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshMinute.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshNsfwInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshTextAnalysisInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshTextExtractionInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshVisionInput.addEventListener('change', saveAutoRefreshSettings);
  // `change`, not `input`: a number box fires `input` on every keystroke, so saving on that would
  // write (and reinstall the scheduled task) for "3" on the way to typing "30".
  els.autoRefreshVisionMinutesInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshGpuWaitInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshLowPriorityInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshToastInput.addEventListener('change', saveAutoRefreshSettings);

  document.addEventListener('keydown', event => {
    // A non-modal dialog gets no free Escape handling from the browser, so this listener is now the
    // only thing closing one on Escape rather than a duplicate of the native behaviour.
    if (event.key === 'Escape') dismissOpenDialogs();
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'r') {
      event.preventDefault();
      refreshAll();
    }
    // The keyboard equivalent of the thumb buttons. Safe inside the search box — Alt+Arrow does
    // nothing to a text field — but not while a dialog owns the screen.
    if (event.altKey && !event.ctrlKey && !event.metaKey &&
        (event.key === 'ArrowLeft' || event.key === 'ArrowRight')) {
      if (document.querySelector('dialog[open]')) return;
      event.preventDefault();
      navigateBy(event.key === 'ArrowLeft' ? -1 : 1);
    }
  });
}

let windowShown = false;

function showWindowAfterPaint() {
  if (windowShown) return;
  windowShown = true;

  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      window.categorizerAPI.showWindow?.()?.catch?.(error => {
        console.warn('Failed to show main window:', error);
      });
    });
  });
}

// Drops from Explorer arrive here rather than as HTML5 drop events, because the Tauri window
// intercepts the OS drag. Dropping only opens the import dialog — nothing is copied until it's
// confirmed, so a stray drag onto the window can't quietly reshape the library.
async function installFileDropListener() {
  try {
    await window.categorizerAPI.onFileDrop(({ state: dropState, paths }) => {
      if (dropState === 'over') {
        els.dropOverlay.classList.remove('hidden');
        return;
      }
      els.dropOverlay.classList.add('hidden');
      if (dropState === 'drop' && paths?.length) openImportDialog(paths);
    });
  } catch (error) {
    console.warn('Failed to install file drop listener:', error);
  }
}

// =================================================================================================
// The automatic-refresh banner
//
// A nightly run is a separate process, so there is no event to subscribe to and nothing in this
// window knows it is happening — that invisibility, and having no way to stop a run from here, is
// what this section exists to fix. The backend answers from a small state file the run heartbeats
// into, and returns null once that heartbeat goes stale, so polling is the whole mechanism.
// =================================================================================================

const AUTO_RUN_POLL_MS = 2000;

function formatAutoRunClock(totalSeconds) {
  const safe = Math.max(0, Math.round(totalSeconds));
  const minutes = Math.floor(safe / 60);
  const seconds = safe % 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}`;
}

function autoRunDetailText(run) {
  const parts = [];
  if (run.total > 0) {
    parts.push(`${run.processed.toLocaleString()} / ${run.total.toLocaleString()}`);
  }
  if (run.rootTotal > 1 && run.rootIndex > 0) {
    parts.push(`folder ${run.rootIndex} of ${run.rootTotal}`);
  }
  if (run.currentName) parts.push(run.currentName);
  return parts.join(' — ');
}

function renderAutoRun(run) {
  if (!run) {
    els.autoRunBanner.classList.add('hidden');
    // The next run gets a live button: leaving it disabled would strand the one control this
    // banner exists to provide.
    els.autoRunStop.disabled = false;
    els.autoRunStop.textContent = 'Stop';
    return;
  }

  els.autoRunBanner.classList.remove('hidden');
  els.autoRunTitle.textContent = run.cancelRequested
    ? 'Automatic refresh — stopping'
    : `Automatic refresh: ${run.label || 'running'}`;
  els.autoRunDetail.textContent = autoRunDetailText(run);

  const hasProgress = run.total > 0;
  els.autoRunTrack.classList.toggle('hidden', !hasProgress);
  if (hasProgress) {
    const percent = Math.min(100, (run.processed / run.total) * 100);
    els.autoRunFill.style.width = `${percent}%`;
  }

  // Only the GPU pass is time-limited, and only once it is actually describing — before that the
  // deadline is 0 and there is nothing honest to count down.
  if (run.visionDeadlineMs > 0) {
    const remainingSeconds = (run.visionDeadlineMs - Date.now()) / 1000;
    els.autoRunLimit.textContent = `${formatAutoRunClock(remainingSeconds)} left of ${run.visionLimitMinutes} min`;
  } else {
    els.autoRunLimit.textContent = '';
  }

  // A pass only notices a stop between images, which for a description is several seconds. Saying
  // so is what keeps the button from looking broken in the gap.
  els.autoRunStop.disabled = run.cancelRequested;
  els.autoRunStop.textContent = run.cancelRequested ? 'Stopping…' : 'Stop';
}

async function pollAutoRun() {
  const wasRunning = Boolean(state.autoRun);
  try {
    state.autoRun = await window.categorizerAPI.getAutoRefreshRun();
  } catch (error) {
    // A failed poll says nothing about whether a run exists, so keep showing the last known state
    // rather than flickering the banner away and back.
    console.warn('Failed to read the automatic refresh state:', error);
    return;
  }
  renderAutoRun(state.autoRun);
  if (state.currentView === 'automation') renderAutoPanel();
  // A run that just ended left behind a new summary and a smaller queue. This is the one moment
  // either can change without the user touching anything, so it is the one moment worth re-reading
  // them — polling for either would be spending a folder read a second on an answer that only moves
  // a few times a day.
  if (wasRunning && !state.autoRun) {
    void loadAutoRefreshSettings();
    void loadAutoRefreshQueue();
  }
}

function installAutoRunBanner() {
  els.autoRunStop.addEventListener('click', async () => {
    els.autoRunStop.disabled = true;
    els.autoRunStop.textContent = 'Stopping…';
    try {
      await window.categorizerAPI.cancelAutoRefreshRun();
      showToast('Stopping the automatic refresh — it finishes the image it is on first.');
    } catch (error) {
      showToast(errorText(error));
      els.autoRunStop.disabled = false;
      els.autoRunStop.textContent = 'Stop';
    }
    // Don't wait out the poll interval to reflect it.
    pollAutoRun();
  });

  pollAutoRun();
  setInterval(pollAutoRun, AUTO_RUN_POLL_MS);
}

// =================================================================================================
// Dashboard
//
// A meta view. It holds no images and answers no question about any single one of them — every
// figure here is a second reading of data the other views already carry, and the point is the SHAPE
// of it: what this library is made of, how far each pass has got through it, and, above all, which
// of it the user did themselves and when.
//
// COMPUTED IN THIS WINDOW, ON EVERY PAINT. `state.library.images` is already here — the scan handed
// it over — and a full pass over 49,837 records measures 3.4 ms. So there is no command, no second
// read of a 28 MB sidecar, and no cache that can be one category assignment out of date. The search
// box is hidden in this view (see `body.view-dashboard`), so the paint-per-keystroke that would make
// a cache worth having cannot happen here.
//
// The ONE thing not computed here is pass coverage's outstanding half: that comes from
// `library.pending`, the backend's own mask table. Those are the passes' eligibility rules — an
// image is skipped for four different reasons and only Rust knows all four — and a second copy of
// them in JavaScript is exactly how a dashboard starts quietly disagreeing with the queue it
// describes.
//
// TWO RULES ABOUT HONESTY, both of which cost real code below:
//   * Counts of DIFFERENT things never share a scale. Hand-sorting and automatic classification get
//     separate strips with their own peaks, because 220 by hand against 48,953 by machine is not a
//     comparison, it is an invisible sliver.
//   * The capture log and the library are never reconciled. The log is the ACT; the library is what
//     SURVIVED. A gap between them is a cleanup, and nothing here may call it missing data — the
//     same rule the Extracted Text panel's strip follows, for the same reason.
// =================================================================================================

// The three the app creates and maintains itself (`ensure_analysis_categories` in lib.rs). Every
// other category in the library is one the user typed, which is the whole distinction the
// "Categories you made yourself" card exists to draw.
const BUILT_IN_CATEGORIES = new Set(['Explicit', 'Low Text', 'High Text']);

// `classified_by` as the backend stamps it. `auto` is the text pass, `auto-nsfw` the explicit one;
// `manual` is only ever written by an assignment the user made, and the auto passes are forbidden
// from overwriting it — which is what makes it a durable record of hand-sorting rather than a
// snapshot of the last scan.
const CLASSIFIER_LABELS = {
  manual: 'By hand',
  auto: 'Text pass',
  'auto-nsfw': 'Explicit pass',
  none: 'Not categorized',
};

function localDayKey(ms) {
  const date = new Date(ms);
  // Local, not UTC: `classified_at` is stored as UTC but the question is which of the user's own
  // days they were sorting on, and slicing the ISO string would move late-evening work to tomorrow.
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;
}

// A continuous run of day keys ending today. Days with nothing are kept, and drawn as empty bars: a
// missing bar and a zero bar are different claims, and only drawing the busy days would quietly
// erase every quiet one — which is most of what a usage pattern actually is.
function recentDayKeys(days) {
  const keys = [];
  const cursor = new Date();
  cursor.setHours(12, 0, 0, 0); // midday, so a DST shift cannot skip or repeat a day
  cursor.setDate(cursor.getDate() - (days - 1));
  for (let index = 0; index < days; index += 1) {
    keys.push(localDayKey(cursor.getTime()));
    cursor.setDate(cursor.getDate() + 1);
  }
  return keys;
}

// One walk of the library, everything at once. Split into more passes it would read better and cost
// as many times more; at 50k records and one paint per view switch, one walk is the right shape.
function computeDashboardStats() {
  const images = state.library?.images || [];
  const stats = {
    total: images.length,
    bytes: 0,
    byClassifier: { manual: 0, auto: 0, 'auto-nsfw': 0, none: 0 },
    categories: new Map(),
    unclassified: 0,
    done: { nsfw: 0, words: 0, extract: 0, described: 0 },
    text: { chars: 0, words: 0, measured: 0, low: 0, high: 0 },
    video: { frames: 0, titles: new Set() },
    descChars: 0,
    folders: new Map(),
    fileFirstMs: 0,
    fileLastMs: 0,
    activity: new Map(),   // day -> { manual, auto }
    activityFirst: null,
    activityLast: null,
  };

  for (const image of images) {
    stats.bytes += image.size || 0;

    const classifier = image.classifiedBy || 'none';
    if (stats.byClassifier[classifier] === undefined) stats.byClassifier[classifier] = 0;
    stats.byClassifier[classifier] += 1;

    if (image.category) {
      stats.categories.set(image.category, (stats.categories.get(image.category) || 0) + 1);
      if (image.category === 'Low Text') stats.text.low += 1;
      if (image.category === 'High Text') stats.text.high += 1;
    } else {
      stats.unclassified += 1;
    }

    if (image.nsfwScore != null) stats.done.nsfw += 1;
    if (image.ocrWordCount != null) {
      stats.done.words += 1;
      stats.text.measured += 1;
      stats.text.words += image.ocrWordCount;
    }
    if (image.ocrTextChars != null) {
      stats.done.extract += 1;
      stats.text.chars += image.ocrTextChars;
    }
    if (image.visionDescChars != null) {
      stats.done.described += 1;
      stats.descChars += image.visionDescChars;
    }
    // Only a real title survives into an ImageView — a frame that was scanned and turned out not to
    // be a video carries `Some("")` in the record and arrives here as null. So this counts video
    // frames, and CANNOT be used to say how many title strips have been read; that number comes off
    // the pending table instead.
    if (image.videoTitle) {
      stats.video.frames += 1;
      stats.video.titles.add(image.videoTitle);
    }

    const folder = image.sourceFolder || 'Root';
    stats.folders.set(folder, (stats.folders.get(folder) || 0) + 1);

    if (image.modifiedMs) {
      if (!stats.fileFirstMs || image.modifiedMs < stats.fileFirstMs) stats.fileFirstMs = image.modifiedMs;
      if (image.modifiedMs > stats.fileLastMs) stats.fileLastMs = image.modifiedMs;
    }

    if (image.classifiedAt) {
      const at = Date.parse(image.classifiedAt);
      if (!Number.isNaN(at)) {
        const key = localDayKey(at);
        const bucket = stats.activity.get(key) || { manual: 0, auto: 0 };
        // Both auto classifiers land in one bucket: the split that matters on this strip is you
        // versus the machine, and which pass the machine used is the coverage card's question.
        if (classifier === 'manual') bucket.manual += 1;
        else bucket.auto += 1;
        stats.activity.set(key, bucket);
        if (!stats.activityFirst || at < stats.activityFirst) stats.activityFirst = at;
        if (!stats.activityLast || at > stats.activityLast) stats.activityLast = at;
      }
    }
  }
  return stats;
}

// Outstanding work per pass, straight off the backend's mask table — the passes' own answer, never
// re-derived here. See the header for why that line is drawn where it is.
function dashPendingFor(bit) {
  const masks = state.library?.pending?.byPassMask;
  if (!masks) return null;
  return masks.reduce((sum, count, mask) => (mask & bit ? sum + count : sum), 0);
}

// --- small builders -----------------------------------------------------------------------------

function dashElement(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

function dashTile(value, label, sub, { tone = '' } = {}) {
  const tile = dashElement('div', `dash-tile${tone ? ` ${tone}` : ''}`);
  tile.append(dashElement('div', 'dash-tile-value', value));
  tile.append(dashElement('div', 'dash-tile-label', label));
  if (sub) {
    const subLine = dashElement('div', 'dash-tile-sub', sub);
    subLine.title = sub;
    tile.append(subLine);
  }
  return tile;
}

// A labelled horizontal bar. `max` is passed in rather than inferred so a group of rows shares one
// scale on purpose, and two groups never share one by accident.
function dashBarRow(label, count, max, { note = '', tone = '', title = '' } = {}) {
  const row = dashElement('div', `dash-bar-row${tone ? ` ${tone}` : ''}`);
  const head = dashElement('div', 'dash-bar-head');
  const name = dashElement('span', 'dash-bar-label', label);
  name.title = title || label;
  head.append(name, dashElement('span', 'dash-bar-count', formatCount(count)));
  const track = dashElement('div', 'dash-bar-track');
  const fill = dashElement('div', 'dash-bar-fill');
  fill.style.width = max > 0 ? `${Math.max(count > 0 ? 1.5 : 0, (count / max) * 100)}%` : '0%';
  track.append(fill);
  row.append(head, track);
  if (note) row.append(dashElement('div', 'dash-bar-note', note));
  return row;
}

// A run of day columns. Every strip carries its own peak in the caption, because two strips on this
// screen never share a scale and a reader who assumes they do would draw the wrong conclusion from
// the taller one.
// A day key as an axis tick. The year is appended only when it is not the current one — on a 90-day
// strip every tick would otherwise carry the same four digits, and on a Year strip that crosses New
// Year the two halves must not be confusable.
function formatAxisDate(dayKey) {
  const [year, month, day] = String(dayKey).split('-').map(Number);
  if (!year || !month || !day) return dayKey;
  const date = new Date(year, month - 1, day);
  const short = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(date);
  return year === new Date().getFullYear() ? short : `${short} ${year}`;
}

// Evenly spaced tick positions, always including the first and last column. `desired` is passed by
// the caller rather than derived from a measured width: the strips live in cards that reflow, and a
// tick count that changed on every resize would be worse than one that is simply chosen for the
// widest a card gets.
function dashAxisIndices(length, desired) {
  if (length <= 1) return [0];
  const count = Math.max(2, Math.min(desired, length));
  const indices = new Set();
  for (let step = 0; step < count; step += 1) {
    indices.add(Math.round((step * (length - 1)) / (count - 1)));
  }
  return [...indices].sort((a, b) => a - b);
}

// How many columns a strip may draw. A bar cannot shrink below 2px without the browser rounding
// some of them away entirely, and with a 1px gap that puts the ceiling at (width + 1) / 3 — about
// 112 columns in the 337px a paired card gets. Measured the hard way: a 365-day strip laid out at
// 1094px and ran clean off the side of the card, taking its axis with it.
const DASH_MAX_COLUMNS = 90;

// Days per column. The ladder is 1 → a week → a month rather than an arbitrary divisor, because a
// column has to be nameable: "Jun 15 – Jun 21" means something and "the fourth 4-day block" does not.
function chooseBucketDays(length) {
  if (length <= DASH_MAX_COLUMNS) return 1;
  if (Math.ceil(length / 7) <= DASH_MAX_COLUMNS) return 7;
  return 30;
}

// Groups a run of daily entries into wider columns, counting BACK from today so the newest column is
// always a whole one. A partial bucket at the recent end would read as a slump that never happened;
// at the old end it is a rendering artifact of a window start nobody chose, and the tooltip names
// the real range either way.
function bucketDayEntries(entries) {
  const bucketDays = chooseBucketDays(entries.length);
  if (bucketDays === 1) return { entries, bucketDays };

  const buckets = [];
  for (let end = entries.length; end > 0; end -= bucketDays) {
    const slice = entries.slice(Math.max(0, end - bucketDays), end);
    const from = slice[0].label;
    const to = slice[slice.length - 1].label;
    buckets.unshift({
      label: from === to ? from : `${from} → ${to}`,
      axis: formatAxisDate(from),
      count: slice.reduce((sum, entry) => sum + entry.count, 0),
    });
  }
  return { entries: buckets, bucketDays };
}

// What a column stands for, said out loud whenever it is not one day. A bar that silently means a
// week is a chart that lies about its own resolution.
function bucketNote(bucketDays) {
  // The axis names each column by its FIRST day, so the rightmost tick reads earlier than the data
  // actually runs — "dated by their first day" is what stops that looking like a chart that stops
  // six days ago.
  if (bucketDays === 7) return 'weekly columns, dated by their first day';
  if (bucketDays > 1) return `${bucketDays}-day columns, dated by their first day`;
  return '';
}

// The axis under a strip. Built as the SAME flex geometry as the bars — one cell per column, same
// gap, same padding — so a tick sits exactly under the column it names. Positioning labels by
// percentage instead would drift against the bars by the accumulated gap, which on a 365-column
// strip is most of a bar.
function dashStripAxis(entries, ticks) {
  const axis = dashElement('div', 'dash-axis');
  const marked = new Set(entries.length ? dashAxisIndices(entries.length, ticks) : []);
  entries.forEach((entry, index) => {
    const cell = dashElement('div', 'dash-axis-cell');
    if (marked.has(index) && entry.axis) {
      cell.classList.add('is-tick');
      // The end labels are pinned to the edges of the box rather than centred on their column: a
      // centred one would hang half its width outside the card and be clipped. The tick MARK stays
      // on the column either way, so nothing about the position is misstated.
      if (index === 0) cell.classList.add('is-first');
      if (index === entries.length - 1) cell.classList.add('is-last');
      cell.append(dashElement('span', null, entry.axis));
    }
    axis.append(cell);
  });
  return axis;
}

function dashDayStrip(entries, { tone = '', emptyText = '', ticks = 4 } = {}) {
  const wrap = dashElement('div', 'dash-strip-wrap');
  const peak = entries.reduce((max, entry) => Math.max(max, entry.count), 0);
  if (!peak) {
    wrap.append(dashElement('p', 'dash-note', emptyText || 'Nothing in this window.'));
    return wrap;
  }
  const strip = dashElement('div', `dash-strip${tone ? ` ${tone}` : ''}`);
  for (const entry of entries) {
    const bar = dashElement('div', 'dash-strip-bar');
    bar.style.setProperty('--h', `${entry.count > 0 ? Math.max(6, (entry.count / peak) * 100) : 0}%`);
    bar.title = `${entry.label} — ${formatCount(entry.count)}`;
    if (entry.count === 0) bar.classList.add('is-empty');
    strip.append(bar);
  }
  wrap.append(strip);
  wrap.append(dashStripAxis(entries, ticks));
  return wrap;
}

// The date behind a peak. "busiest day 7,765" is a number you cannot look up anywhere else in the
// app, and on a 365-column strip you cannot read its position off the axis either.
function dashPeakEntry(entries) {
  return entries.reduce((best, entry) => (entry.count > (best?.count ?? 0) ? entry : best), null);
}

function dashCaption(text) {
  return dashElement('p', 'dash-caption', text);
}

// --- the cards ----------------------------------------------------------------------------------

function renderDashboardTiles(stats) {
  const container = els.dashTiles;
  container.innerHTML = '';
  const share = (count) => (stats.total ? `${Math.round((count / stats.total) * 100)}%` : '—');

  container.append(dashTile(formatCount(stats.total), 'Images', formatBytes(stats.bytes)));
  container.append(dashTile(
    share(stats.total - stats.unclassified),
    'Categorized',
    `${formatCount(stats.unclassified)} still unclassified`,
  ));
  container.append(dashTile(
    formatCount(stats.byClassifier.manual || 0),
    'Sorted by hand',
    stats.total ? `${((stats.byClassifier.manual || 0) / stats.total * 100).toFixed(1)}% of the library` : '',
  ));
  container.append(dashTile(
    share(stats.done.described),
    'Described',
    `${formatCount(stats.done.described)} images · ${countLabel(stats.descChars)} characters`,
  ));
  container.append(dashTile(
    countLabel(stats.text.chars),
    'Characters of text read',
    `from ${formatCount(stats.done.extract)} images`,
  ));
  container.append(dashTile(
    formatCount(stats.video.titles.size),
    'Videos recognised',
    `${formatCount(stats.video.frames)} frames · ${formatCount(stats.total - stats.video.frames)} stills`,
  ));
}

function renderDashboardUsage(stats) {
  const container = els.dashUsage;
  container.innerHTML = '';

  const rows = Object.entries(stats.byClassifier)
    .filter(([, count]) => count > 0)
    .sort((a, b) => b[1] - a[1]);
  const max = rows.reduce((peak, [, count]) => Math.max(peak, count), 0);
  for (const [key, count] of rows) {
    container.append(dashBarRow(CLASSIFIER_LABELS[key] || key, count, max, {
      tone: key === 'manual' ? 'is-manual' : '',
      title: key === 'manual'
        ? 'A category you assigned yourself. The automatic passes are forbidden from overwriting one, so this survives every rescan.'
        : 'Assigned by an analysis pass, and reassigned whenever the thresholds move.',
    }));
  }

  const manual = stats.byClassifier.manual || 0;
  container.append(dashCaption(manual
    ? `${formatCount(manual)} of ${formatCount(stats.total)} images carry a category you chose — `
      + `${(manual / Math.max(1, stats.total) * 100).toFixed(1)}%. The rest were classified by a pass, `
      + 'and would be reclassified if you moved a threshold.'
    : 'Nothing has been categorized by hand yet — every category in this library was assigned by a pass.'));
}

function renderDashboardOwnCategories(stats) {
  const container = els.dashOwn;
  container.innerHTML = '';

  const own = [...stats.categories.entries()]
    .filter(([name]) => !BUILT_IN_CATEGORIES.has(name))
    .sort((a, b) => b[1] - a[1]);
  const builtIn = [...stats.categories.entries()].filter(([name]) => BUILT_IN_CATEGORIES.has(name));
  const builtInTotal = builtIn.reduce((sum, [, count]) => sum + count, 0);

  if (!own.length) {
    container.append(dashElement('p', 'dash-note',
      'None yet. Every category here is one the app maintains itself — add one from the sidebar and '
      + 'whatever you file into it shows up on this card.'));
  } else {
    const max = own.reduce((peak, [, count]) => Math.max(peak, count), 0);
    for (const [name, count] of own) {
      container.append(dashBarRow(name, count, max, { tone: 'is-manual' }));
    }
    container.append(dashCaption(
      `${own.length} categor${own.length === 1 ? 'y' : 'ies'} of your own, holding `
      + `${formatCount(own.reduce((sum, [, count]) => sum + count, 0))} images.`));
  }

  container.append(dashElement('p', 'dash-note',
    `${builtIn.length} built-in categor${builtIn.length === 1 ? 'y' : 'ies'} `
    + `(${builtIn.map(([name]) => name).join(', ') || 'none yet'}) hold ${formatCount(builtInTotal)} — `
    + 'those are maintained by the passes and move on their own when a threshold changes.'));
}

function renderDashboardActivity(stats) {
  const container = els.dashActivity;
  container.innerHTML = '';

  const days = recentDayKeys(state.dashDays);
  const entryFor = (day, key) => ({ label: day, axis: formatAxisDate(day), count: stats.activity.get(day)?.[key] || 0 });
  const manualEntries = days.map(day => entryFor(day, 'manual'));
  const autoEntries = days.map(day => entryFor(day, 'auto'));
  const manualTotal = manualEntries.reduce((sum, entry) => sum + entry.count, 0);
  const autoTotal = autoEntries.reduce((sum, entry) => sum + entry.count, 0);
  const manualPeak = dashPeakEntry(manualEntries);
  const autoPeak = dashPeakEntry(autoEntries);
  const manualDays = manualEntries.filter(entry => entry.count > 0).length;
  // The chart may be bucketed; every NUMBER below still comes off the raw days, so "busiest Jun 17
  // with 62" stays a statement about a day even when the bar beside it covers a week.
  const manualStrip = bucketDayEntries(manualEntries);
  const autoStrip = bucketDayEntries(autoEntries);
  const bucketing = bucketNote(manualStrip.bucketDays);

  // ⚠ Two strips, two scales, and the peaks stated on both. Stacked into one chart the hand-sorted
  // bars would round to nothing against a pass that classified 7,765 images in a day — the reader
  // would conclude they never sort anything by hand, which is the opposite of what the data says.
  const bench = dashElement('div', 'dash-strip-pair');

  const manualBlock = dashElement('div', 'dash-strip-block');
  manualBlock.append(dashElement('div', 'dash-strip-title', 'Sorted by hand'));
  manualBlock.append(dashDayStrip(manualStrip.entries, {
    tone: 'is-manual',
    emptyText: 'No hand-sorting in this window.',
    ticks: 3,
  }));
  manualBlock.append(dashCaption(manualTotal
    ? `${formatCount(manualTotal)} across ${manualDays} day${manualDays === 1 ? '' : 's'} · `
      + `busiest ${formatAxisDate(manualPeak.label)} with ${formatCount(manualPeak.count)}`
      + (bucketing ? ` · ${bucketing}` : '')
    : 'Nothing in this window.'));
  bench.append(manualBlock);

  const autoBlock = dashElement('div', 'dash-strip-block');
  autoBlock.append(dashElement('div', 'dash-strip-title', 'Classified by a pass'));
  autoBlock.append(dashDayStrip(autoStrip.entries, {
    emptyText: 'No passes classified anything in this window.',
    ticks: 3,
  }));
  autoBlock.append(dashCaption(autoTotal
    ? `${formatCount(autoTotal)} · busiest ${formatAxisDate(autoPeak.label)} with ${formatCount(autoPeak.count)}`
      + ' — its own scale, not the one above'
    : 'Nothing in this window.'));
  bench.append(autoBlock);

  container.append(bench);

  const notes = [];
  if (stats.activityLast) notes.push(`Last category assigned ${formatDate(stats.activityLast)}`);
  if (stats.activityFirst) notes.push(`first ${formatDate(stats.activityFirst)}`);
  container.append(dashCaption(
    `${notes.join(' · ')}${notes.length ? '. ' : ''}`
    + 'A day is counted when an image CHANGED category, not when a scan looked at it — a pass that '
    + 'confirms what it already decided leaves no mark here.'));
}

function renderDashboardCoverage(stats) {
  const container = els.dashCoverage;
  container.innerHTML = '';

  const eligible = state.library?.pending?.eligibleImages ?? 0;
  const chunkPending = dashPendingFor(2);
  const rows = [
    { label: 'Explicit scored', done: stats.done.nsfw, pending: dashPendingFor(1) },
    { label: 'Text measured', done: stats.done.words, pending: dashPendingFor(4) },
    { label: 'Text extracted', done: stats.done.extract, pending: dashPendingFor(8) },
    // No record field can answer this one: a frame that was scanned and is not a video stores an
    // empty title, which the view collapses to null. So it is derived the only honest way available,
    // off the pool the pass actually draws from.
    { label: 'Video titles read', done: chunkPending == null ? null : Math.max(0, eligible - chunkPending), pending: chunkPending },
    { label: 'Described', done: stats.done.described, pending: dashPendingFor(16) },
  ];

  for (const row of rows) {
    if (row.done == null || row.pending == null) continue;
    // Share of the work this pass HAS, never of the whole library: Describe deliberately skips
    // 12,000 deduped video frames, and counting those against it would report it as half finished
    // when it has almost nothing left to do.
    const scope = row.done + row.pending;
    // Held below 100 while anything is outstanding. 49,829 of 49,843 rounds to 100%, and "100% ·
    // 14 still to do" on one line reads as a contradiction rather than as a rounding.
    const rounded = scope > 0 ? Math.round((row.done / scope) * 100) : 100;
    const pct = row.pending > 0 ? Math.min(99, rounded) : 100;
    container.append(dashBarRow(row.label, row.done, Math.max(1, scope), {
      note: row.pending > 0
        ? `${pct}% · ${formatCount(row.pending)} still to do`
        : `${pct}% · nothing outstanding`,
      tone: row.pending > 0 ? '' : 'is-clear',
      title: `${formatCount(row.done)} done, ${formatCount(row.pending)} outstanding, out of the ${formatCount(scope)} images this pass covers.`,
    }));
  }

  const skips = state.library?.pending;
  if (skips) {
    const skipped = (skips.visionSkippedVideo || 0) + (skips.visionSkippedExplicit || 0)
      + (skips.visionSkippedCategory || 0) + (skips.visionSkippedUnscored || 0);
    if (skipped > 0) {
      container.append(dashCaption(
        `Describe leaves ${formatCount(skipped)} images alone on purpose: `
        + `${formatCount(skips.visionSkippedVideo)} duplicate video frames, `
        + `${formatCount(skips.visionSkippedCategory)} in omitted categories, `
        + `${formatCount(skips.visionSkippedExplicit)} explicit, `
        + `${formatCount(skips.visionSkippedUnscored)} not yet scored. They are not counted above.`));
    }
  }
}

function renderDashboardContents(stats) {
  const container = els.dashContents;
  container.innerHTML = '';

  const categories = [...stats.categories.entries()].sort((a, b) => b[1] - a[1]);
  const max = Math.max(stats.unclassified, ...categories.map(([, count]) => count), 1);
  for (const [name, count] of categories) {
    container.append(dashBarRow(name, count, max, {
      tone: BUILT_IN_CATEGORIES.has(name) ? '' : 'is-manual',
      title: BUILT_IN_CATEGORIES.has(name) ? `${name} — maintained by a pass` : `${name} — your own category`,
    }));
  }
  if (stats.unclassified > 0) {
    container.append(dashBarRow('Unclassified', stats.unclassified, max, { tone: 'is-muted' }));
  }

  const avgWords = stats.text.measured ? Math.round(stats.text.words / stats.text.measured) : 0;
  const facts = [
    `${formatCount(avgWords)} words of text on a typical image`,
    stats.video.frames
      ? `${formatCount(stats.video.frames)} frames belong to ${formatCount(stats.video.titles.size)} videos`
      : null,
    stats.fileFirstMs
      ? `Files span ${formatDate(stats.fileFirstMs)} → ${formatDate(stats.fileLastMs)}`
      : null,
  ].filter(Boolean);
  container.append(dashCaption(facts.join(' · ')));
}

function renderDashboardFolders(stats) {
  const container = els.dashFolders;
  container.innerHTML = '';

  // Sorted by NAME, not by size: these folders are dated here, so name order is time order and the
  // card doubles as a timeline of when material arrived. Sorting by count would destroy that.
  const folders = [...stats.folders.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  const max = folders.reduce((peak, [, count]) => Math.max(peak, count), 0);
  for (const [name, count] of folders) {
    container.append(dashBarRow(name, count, max));
  }
  container.append(dashCaption(
    `${folders.length} source folder${folders.length === 1 ? '' : 's'} · `
    + `${formatBytes(stats.bytes)} in total. Folders switched off for analysis still appear here — `
    + 'this is what is on disk, not what the passes look at.'));
}

// --- the capture log ----------------------------------------------------------------------------
//
// A SECOND, INDEPENDENT source, and the only one on this screen that is not about the library. It
// records the act of taking a screenshot; the library records what survived. The two are never
// reconciled — a month cleared out of the save folder is gone from every other figure here and must
// not be gone from a record of the user's own habit. See `capture_log.rs` and the Extracted Text
// panel's strip, which follows the identical rule.

async function loadDashboardCaptures({ force = false } = {}) {
  if (state.dashCapturesLoading) return;
  if (state.dashCaptures !== undefined && state.dashCaptures?.days === state.dashDays && !force) return;
  state.dashCapturesLoading = true;
  try {
    state.dashCaptures = await window.categorizerAPI.getCaptureActivity(state.dashDays);
  } catch (error) {
    // A machine without screenshot-tool is the normal case, not an error worth a toast.
    console.warn('capture log unavailable', error);
    state.dashCaptures = { blocked: 'not_installed' };
  } finally {
    state.dashCapturesLoading = false;
  }
  if (state.currentView === 'dashboard') renderDashboard();
}

function renderDashboardCaptures() {
  const container = els.dashCaptures;
  container.innerHTML = '';
  const data = state.dashCaptures;

  if (!data) {
    container.append(dashElement('p', 'dash-note', 'Reading the capture log…'));
    return;
  }
  if (data.blocked) {
    // Four different reasons for "nothing here", and collapsing any two says something false about
    // either the tool or the user. `not_installed` deliberately explains nothing at all.
    const text = captureBlockedText(data.blocked);
    container.append(dashElement('p', 'dash-note',
      text || 'Screenshot Tool is not on this machine, so there is no record of captures to read.'));
    return;
  }

  // `by_day` holds ONLY the days that had captures, so drawing its entries directly would space 12
  // scattered days evenly across the width and read as 12 consecutive ones. The strip is rebuilt
  // over a continuous run of days instead, with the quiet ones drawn as floors.
  //
  // Clamped to the log's own lifetime, not to the window: a log that started 11 days ago has
  // nothing to say about the 79 days before it, and drawing those as empty would assert the user
  // captured nothing on days that were never observed. `covers_from` is the log's first day.
  const windowKeys = recentDayKeys(state.dashDays);
  const logStart = data.coverage?.covers_from || Object.keys(data.by_day || {})[0];
  const dayKeys = logStart ? windowKeys.filter(key => key >= logStart) : windowKeys;
  const captureStrip = bucketDayEntries(
    dayKeys.map(key => ({ label: key, axis: formatAxisDate(key), count: data.by_day?.[key] || 0 })),
  );
  container.append(dashDayStrip(captureStrip.entries, {
    // A full-width card, so it can carry twice the ticks the paired strips above can.
    tone: 'is-capture',
    emptyText: 'No captures recorded in this window.',
    ticks: 6,
  }));

  // ⚠ The period has to be the LOG's, not the window's, whenever the log is younger. "4,098 in 90
  // days" off eleven days of history is a rate understated eightfold, and nothing else on screen
  // would contradict it — the coverage caveat below only fires under 50%.
  const spanDays = dayKeys.length;
  const parts = [data.coverage?.truncated_to_log
    ? `${formatCount(data.total)} taken over the log's ${spanDays} day${spanDays === 1 ? '' : 's'} of history`
    : `${formatCount(data.total)} taken in ${data.days} days`];
  if (data.active_days) {
    parts.push(data.active_days >= spanDays
      ? 'on every one of them'
      : `on ${formatCount(data.active_days)} of those days`);
  }
  if (data.median_per_active_day != null) {
    // Stated as this user's own baseline and never as a verdict: "a lot" only means anything against
    // their own habit, and the app has no business ranking their day.
    parts.push(`typically ${formatCount(data.median_per_active_day)} on a day you capture`);
  }
  if (data.total_on_record && data.total_on_record !== data.total) {
    parts.push(`${formatCount(data.total_on_record)} on record all-time`);
  }
  const captureBucketing = bucketNote(captureStrip.bucketDays);
  if (captureBucketing) parts.push(captureBucketing);
  container.append(dashCaption(parts.join(' · ')));

  const hours = data.by_hour || [];
  if (hours.some(count => count > 0)) {
    const block = dashElement('div', 'dash-strip-block');
    block.append(dashElement('div', 'dash-strip-title', 'Hour of the day'));
    block.append(dashDayStrip(
      hours.map((count, hour) => {
        const label = `${String(hour).padStart(2, '0')}:00`;
        // Every third hour: 24 columns take a tick each three across a wide card without touching.
        return { label, axis: hour % 3 === 0 ? label : '', count };
      }),
      { tone: 'is-capture', ticks: 24 },
    ));
    // The prose that used to say which end was which is gone — the axis says it, and in more places.
    if (data.peak_hour != null) {
      block.append(dashCaption(`Busiest around ${String(data.peak_hour).padStart(2, '0')}:00`));
    }
    container.append(block);
  }

  const modes = Object.entries(data.by_mode || {}).sort((a, b) => b[1] - a[1]);
  if (modes.length) {
    const max = modes[0][1];
    const block = dashElement('div', 'dash-strip-block');
    block.append(dashElement('div', 'dash-strip-title', 'How you capture'));
    for (const [mode, count] of modes) block.append(dashBarRow(mode, count, max, { tone: 'is-capture' }));
    container.append(block);
  }

  // ⚠ Every zero qualified before it is read. The tool records only while it runs, so a low count
  // has three possible meanings and only one of them is about the user.
  if (data.coverage && data.coverage.coverage_pct < 50) {
    if (!data.coverage.conclusive) {
      container.append(dashElement('p', 'dash-note',
        'Coverage not yet established — treat low counts here as unknown, not as zero.'));
    } else {
      const period = data.coverage.truncated_to_log
        ? `the log's ${humanizeMinutes(data.coverage.window_min)} of history`
        : 'this window';
      container.append(dashElement('p', 'dash-note',
        `Screenshot Tool was open for ${data.coverage.coverage_pct}% of ${period}, so this is a sample `
        + 'of when it was running, not of the period.'));
    }
  }
  if (data.failed > 0) {
    container.append(dashElement('p', 'dash-note',
      `${formatCount(data.failed)} captures never landed a file — expected to be missing from the library.`));
  }
  container.append(dashElement('p', 'dash-note',
    'From Screenshot Tool\'s own log, not from this library: it records the act, so it does not fall '
    + 'when you delete or move pictures. The two are meant to disagree.'));
}

// --- entry points -------------------------------------------------------------------------------

function renderDashboard() {
  for (const button of els.dashRange.querySelectorAll('.dash-range-button')) {
    button.classList.toggle('is-active', Number(button.dataset.days) === state.dashDays);
  }

  if (!state.library) {
    els.dashTiles.innerHTML = '';
    for (const node of [els.dashUsage, els.dashOwn, els.dashActivity, els.dashCoverage, els.dashContents, els.dashFolders]) {
      node.innerHTML = '';
      node.append(dashElement('p', 'dash-note', 'Choose a root folder to measure.'));
    }
    els.dashAsOf.textContent = '';
    renderDashboardCaptures();
    return;
  }

  const stats = computeDashboardStats();
  els.dashAsOf.textContent = `${formatCount(stats.total)} images · measured at the last scan`;
  renderDashboardTiles(stats);
  renderDashboardUsage(stats);
  renderDashboardOwnCategories(stats);
  renderDashboardActivity(stats);
  renderDashboardCaptures();
  renderDashboardCoverage(stats);
  renderDashboardContents(stats);
  renderDashboardFolders(stats);
}

function selectDashboard() {
  cancelPointerDrag();
  pushNavEntry(navEntry('dashboard'));
  state.currentView = 'dashboard';
  state.currentCategory = null;
  render();
  void loadDashboardCaptures();
}

function installDashboard() {
  els.dashboardTab.addEventListener('click', selectDashboard);
  els.dashRange.addEventListener('click', event => {
    const button = event.target.closest('.dash-range-button');
    if (!button) return;
    const days = Number(button.dataset.days);
    if (!days || days === state.dashDays) return;
    state.dashDays = days;
    render();
    // The library half re-answers instantly off records already here; only the log has to be re-read.
    void loadDashboardCaptures();
  });
  els.dashRefreshButton.addEventListener('click', async () => {
    els.dashRefreshButton.disabled = true;
    try {
      await refreshAll();
      await loadDashboardCaptures({ force: true });
    } finally {
      els.dashRefreshButton.disabled = false;
    }
  });
}

// =================================================================================================
// Automation
//
// The scheduled run is the one part of this app that acts while nobody is watching, and everything
// about it used to be split between a banner that only exists mid-run and a paragraph at the bottom
// of a dialog. This panel puts the three questions in one place, in the order they get asked: is it
// on and what is it doing, what is waiting for it, and what should it be doing.
//
// The controls in the third card were MOVED here from Settings rather than mirrored. Two live
// copies of one schedule is a way to set it twice and mean neither, and duplicate element ids are
// not a thing the DOM offers anyway.
//
// The queue is the part worth being careful about. It is counted from each folder's stored records,
// not from the disk: a scan of a 90k-image root takes minutes, which is the right price for a run's
// first act and the wrong one for opening a tab. So every number here is "as of the last scan", the
// panel says so in as many words, and nothing pretends a count of an unscanned folder is zero.
// =================================================================================================

const AUTO_PASS_LABEL = { nsfw: 'Explicit', text: 'Text', ocr: 'Extract Text', vision: 'Describe' };
const AUTO_PASS_BIT = { nsfw: 1, text: 4, ocr: 8, vision: 16 };

function selectAutomation() {
  cancelPointerDrag();
  pushNavEntry(navEntry('automation'));
  state.currentView = 'automation';
  state.currentCategory = null;
  render();
  // Both are cheap and both go stale on their own clock — the task can be disabled in Task
  // Scheduler, and a run that finished since the last look rewrote the queue and the summary.
  void loadAutoRefreshSettings();
  void loadAutoRefreshQueue();
}

async function loadAutoRefreshQueue() {
  try {
    state.autoQueue = await window.categorizerAPI.getAutoRefreshQueue();
  } catch (error) {
    // A failed read is not "nothing queued" — leave the last known counts up rather than replacing
    // them with a zero that would read as an answer.
    console.warn('Failed to read the scheduled queue:', error);
    return;
  }
  renderSidebar();
  if (state.currentView === 'automation') renderAutoPanel();
}

// The union over the passes a run is set to perform, for one folder — same mask arithmetic as the
// Analyze row's readout, and same reason: an image new to two passes is one image to process.
function autoQueuePassCount(folder, passes) {
  const masks = folder?.pending?.byPassMask;
  if (!masks) return 0;
  const selection = passes.reduce((bits, pass) => bits | (AUTO_PASS_BIT[pass] || 0), 0);
  return masks.reduce((sum, count, mask) => (mask & selection ? sum + count : sum), 0);
}

function autoFactRow(term, detail, { tone = '' } = {}) {
  const row = document.createElement('div');
  row.className = `auto-fact${tone ? ` ${tone}` : ''}`;
  const dt = document.createElement('dt');
  dt.textContent = term;
  const dd = document.createElement('dd');
  dd.textContent = detail;
  dd.title = detail;
  row.append(dt, dd);
  return row;
}

function renderAutoFacts() {
  const auto = state.autoRefresh;
  els.autoFacts.innerHTML = '';
  if (!auto) {
    els.autoFacts.append(autoFactRow('Schedule', 'Could not be read.', { tone: 'is-warn' }));
    return;
  }

  els.autoFacts.append(
    auto.enabled
      ? autoFactRow('Schedule', `On — every day at ${auto.time}`)
      : autoFactRow('Schedule', 'Off — nothing runs on its own', { tone: 'is-off' }),
  );

  // The OS's own answer, not this app's opinion of it: a task disabled or deleted in Task Scheduler
  // is invisible to the toggle above, and that gap is exactly what this row is for.
  if (auto.taskInstalled) {
    const detail = [auto.taskNextRun ? `next ${auto.taskNextRun}` : null, auto.taskStatus]
      .filter(Boolean)
      .join(' · ');
    els.autoFacts.append(autoFactRow('Windows task', detail ? `Installed — ${detail}` : 'Installed'));
  } else {
    els.autoFacts.append(autoFactRow(
      'Windows task',
      auto.enabled ? 'Not installed — nothing will fire on its own' : 'Not installed',
      { tone: auto.enabled ? 'is-warn' : 'is-off' },
    ));
  }

  const folders = auto.roots || [];
  els.autoFacts.append(folders.length
    ? autoFactRow(`Folder${folders.length === 1 ? '' : 's'}`, folders.join(' · '))
    : autoFactRow('Folders', 'None picked', { tone: auto.enabled ? 'is-warn' : 'is-off' }));

  const passes = (state.autoQueue?.scheduledPasses || []).map(pass => AUTO_PASS_LABEL[pass] || pass);
  els.autoFacts.append(passes.length
    ? autoFactRow('Passes', passes.join(' · '))
    : autoFactRow('Passes', 'None — a run would only rescan for new files', { tone: 'is-off' }));

  if (auto.runVision) {
    els.autoFacts.append(autoFactRow(
      'Describe limit',
      auto.visionMinutes > 0
        ? `${auto.visionMinutes} min of GPU time per run${auto.gpuWait ? ', after the card is free' : ''}`
        : 'No limit — it runs the backlog down',
      { tone: auto.visionMinutes > 0 ? '' : 'is-warn' },
    ));
  }

  if (auto.lastRunAt) {
    els.autoFacts.append(autoFactRow(
      'Last run',
      `${formatDate(Date.parse(auto.lastRunAt))} — ${auto.lastRunSummary || 'no summary recorded'}`,
      // A run that found nothing is the uneventful outcome, not a result to be read twice.
      { tone: auto.lastRunNoWork ? 'is-clear' : '' },
    ));
  } else {
    els.autoFacts.append(autoFactRow('Last run', 'Never'));
  }
}

function autoQueueNote(text, tone = '') {
  const note = document.createElement('p');
  note.className = `auto-queue-headline${tone ? ` ${tone}` : ''}`;
  note.textContent = text;
  return note;
}

function renderAutoQueueFolder(folder, passes) {
  const row = document.createElement('div');
  row.className = 'auto-queue-row';

  const path = document.createElement('div');
  path.className = 'auto-queue-path';
  path.textContent = folder.root;
  path.title = folder.root;
  row.append(path);

  const body = document.createElement('div');
  body.className = 'auto-queue-body';

  if (!folder.exists) {
    body.append(autoQueueNote('Folder not found — the run skips it.', 'is-warn'));
    row.append(body);
    return row;
  }
  if (!folder.scanned) {
    // Refusing to say "0" here is the point: nothing has ever been counted in this folder, and a
    // zero would be indistinguishable from one that is genuinely up to date.
    body.append(autoQueueNote('Never scanned — the run scans it first, then works through whatever it finds.'));
    row.append(body);
    return row;
  }

  const chips = document.createElement('div');
  chips.className = 'auto-queue-chips';
  for (const pass of passes) {
    const count = autoQueuePassCount(folder, [pass]);
    const chip = document.createElement('span');
    chip.className = `auto-queue-chip${count ? '' : ' is-clear'}`;
    chip.textContent = `${AUTO_PASS_LABEL[pass] || pass} ${formatCount(count)}`;
    if (pass === 'vision' && folder.visionUnlockable > 0) {
      // Describe's count is a snapshot taken before scoring: an unscored image is invisible to it
      // and becomes its work later in the very same run. "Describe 0" out of thousands reads as a
      // bug until the number that went missing is on screen too.
      chip.textContent += ` (+${formatCount(folder.visionUnlockable)} once scored)`;
      chip.classList.remove('is-clear');
    }
    chips.append(chip);
  }
  body.append(chips);

  const meta = document.createElement('div');
  meta.className = 'auto-queue-meta';
  const counted = folder.countedAtMs ? `counted ${formatDate(folder.countedAtMs)}` : 'never counted';
  meta.textContent = `${formatCount(folder.knownImages)} images known · ${counted}`;
  body.append(meta);

  row.append(body);
  return row;
}

function renderAutoQueue() {
  const queue = state.autoQueue;
  const container = els.autoQueue;
  container.innerHTML = '';
  if (!queue) {
    container.append(autoQueueNote('Not counted yet.'));
    return;
  }

  const passes = queue.scheduledPasses || [];
  const folders = queue.folders || [];

  // Three different kinds of "nothing", and collapsing them into one number is how a schedule that
  // cannot work reads as a schedule with nothing to do.
  if (!queue.enabled) {
    container.append(autoQueueNote('Automatic runs are off, so nothing is queued.', 'is-off'));
  } else if (!folders.length) {
    container.append(autoQueueNote('No folders are picked, so a run would have nothing to look at.', 'is-warn'));
  } else if (!passes.length) {
    container.append(autoQueueNote('No passes are ticked — a run would rescan for new files and stop there.', 'is-off'));
  } else if (queue.nothingToDo) {
    container.append(autoQueueNote(
      'Nothing to process. Every folder is up to date for the passes that are scheduled, so the next run will '
      + 'rescan for new files, find nothing to do, and finish.',
      'is-clear',
    ));
  } else {
    container.append(autoQueueNote(
      `${formatCount(queue.scheduledPending)} image${queue.scheduledPending === 1 ? '' : 's'} queued for the next run.`,
    ));
  }

  for (const folder of folders) {
    container.append(renderAutoQueueFolder(folder, passes));
  }
}

function renderAutoLive() {
  const run = state.autoRun;
  els.autoLive.classList.toggle('hidden', !run);
  els.autoStopButton.classList.toggle('hidden', !run);
  // Run Now would launch a second process behind the one already going, and the backend refuses it
  // anyway — saying so with the button is cheaper than saying it with an error.
  els.autoRunNowButton.disabled = Boolean(run);
  if (!run) return;

  els.autoLiveLabel.textContent = run.cancelRequested ? 'Stopping…' : (run.label || 'Running');
  els.autoLiveDetail.textContent = [
    autoRunDetailText(run),
    run.startedMs ? `started ${formatDate(run.startedMs)}` : '',
  ].filter(Boolean).join(' — ');

  const hasProgress = run.total > 0;
  els.autoLiveTrack.classList.toggle('hidden', !hasProgress);
  if (hasProgress) els.autoLiveFill.style.width = `${Math.min(100, (run.processed / run.total) * 100)}%`;

  els.autoLiveLimit.textContent = run.visionDeadlineMs > 0
    ? `${formatAutoRunClock((run.visionDeadlineMs - Date.now()) / 1000)} left of ${run.visionLimitMinutes} min`
    : '';
  els.autoStopButton.disabled = Boolean(run.cancelRequested);
  els.autoStopButton.textContent = run.cancelRequested ? 'Stopping…' : '■ Stop Run';
}

function renderAutoPanel() {
  const auto = state.autoRefresh;
  const queue = state.autoQueue;

  if (state.autoRun) {
    els.autoStateChip.textContent = 'Running now';
    els.autoStateChip.className = 'auto-state-chip is-running';
  } else if (!auto?.enabled) {
    els.autoStateChip.textContent = 'Off';
    els.autoStateChip.className = 'auto-state-chip is-off';
  } else if (queue?.nothingToDo) {
    els.autoStateChip.textContent = 'On — nothing queued';
    els.autoStateChip.className = 'auto-state-chip is-clear';
  } else if (queue?.scheduledPending) {
    els.autoStateChip.textContent = `On — ${formatCount(queue.scheduledPending)} queued`;
    els.autoStateChip.className = 'auto-state-chip is-on';
  } else {
    els.autoStateChip.textContent = 'On';
    els.autoStateChip.className = 'auto-state-chip is-on';
  }

  const counted = (queue?.folders || [])
    .map(folder => folder.countedAtMs)
    .filter(Boolean)
    .sort((a, b) => a - b)[0];
  els.autoCounted.textContent = counted ? `Counts as of ${formatDate(counted)}` : '';

  renderAutoLive();
  renderAutoFacts();
  renderAutoQueue();
}

function installAutomationPanel() {
  els.automationTab.addEventListener('click', selectAutomation);
  els.openAutomationButton.addEventListener('click', () => {
    closeSettingsDialog();
    selectAutomation();
  });

  els.autoRecheckButton.addEventListener('click', async () => {
    els.autoRecheckButton.disabled = true;
    try {
      await loadAutoRefreshSettings();
      await loadAutoRefreshQueue();
    } finally {
      els.autoRecheckButton.disabled = false;
    }
  });

  els.autoRunNowButton.addEventListener('click', async () => {
    els.autoRunNowButton.disabled = true;
    try {
      await window.categorizerAPI.runAutoRefreshNow();
      showToast('Started the scheduled run — it reports itself here and in the banner within a couple of seconds.');
    } catch (error) {
      showToast(errorText(error));
      els.autoRunNowButton.disabled = false;
      return;
    }
    // The run publishes its first state a moment after launching; the poll is what picks it up, and
    // firing one now saves the panel looking inert for a whole interval.
    setTimeout(pollAutoRun, 600);
  });

  els.autoStopButton.addEventListener('click', async () => {
    els.autoStopButton.disabled = true;
    els.autoStopButton.textContent = 'Stopping…';
    try {
      await window.categorizerAPI.cancelAutoRefreshRun();
      showToast('Stopping the automatic run — it finishes the image it is on first.');
    } catch (error) {
      showToast(errorText(error));
    }
    pollAutoRun();
  });
}

async function installAnalysisListeners() {
  const listeners = [
    window.categorizerAPI.onTextAnalysisProgress(({ processed, total, currentName }) => {
      setStatus(`Text: ${processed}/${total} — ${currentName}`);
    }),
    window.categorizerAPI.onTextAnalysisFinished(payload => onAnalysisFinished('text', payload)),
    window.categorizerAPI.onNsfwAnalysisProgress(({ processed, total, currentName }) => {
      setStatus(`Explicit: ${processed}/${total} — ${currentName}`);
    }),
    window.categorizerAPI.onNsfwAnalysisFinished(payload => onAnalysisFinished('nsfw', payload)),
    window.categorizerAPI.onTextExtractionProgress(({ processed, total, currentName }) => {
      setStatus(`Extract Text: ${processed}/${total} — ${currentName}`);
    }),
    window.categorizerAPI.onTextExtractionFinished(payload => onAnalysisFinished('ocr', payload)),
    window.categorizerAPI.onChunkScanProgress(({ processed, total, currentName }) => {
      setStatus(`Video Dedup: ${processed}/${total} — ${currentName}`);
    }),
    window.categorizerAPI.onChunkScanFinished(payload => onAnalysisFinished('chunk', payload)),
    window.categorizerAPI.onVisionAnalysisProgress(({ processed, total, currentName }) => {
      setStatus(`Describe: ${processed}/${total} — ${currentName}`);
    }),
    window.categorizerAPI.onVisionAnalysisFinished(payload => onAnalysisFinished('vision', payload)),
  ];

  const results = await Promise.allSettled(listeners);
  for (const result of results) {
    if (result.status === 'rejected') {
      console.warn('Failed to install analysis listener:', result.reason);
    }
  }
}

async function init() {
  try {
    installEvents();
    installTitlebar();
    installDialogDismissal();
    // Mark loading before the first paint so the window unhides onto an intentional spinner
    // rather than the "No root folder chosen yet" empty state shown before settings load.
    setLoading(true);
    render();
    showWindowAfterPaint();

    await installAnalysisListeners();
    await installKindListeners();
    await installTopicListeners();
    await installFileDropListener();
    installAutoRunBanner();
    // Fire-and-forget beside the library scan: neither reads the root, and the sidebar's Automation
    // pill is meant to answer "is anything queued for tonight" without the tab ever being opened.
    void loadAutoRefreshSettings();
    void loadAutoRefreshQueue();
    await refreshAll();
  } catch (error) {
    setLoading(false);
    console.error('Startup failed:', error);
    setStatus('Startup hit an error. Choose or rescan a folder to retry.');
    showToast(errorText(error));
    render();
    showWindowAfterPaint();
  }
}

// ---------------------------------------------------------------------------------------------
// Extracted Text
//
// Reads the same index `icat` reads. Two rules shape everything here:
//   * Opening the panel must never start work — `getTextStatus` reports what exists and offers a
//     button; it does not build. Searching may build, because that is what the user just asked for.
//   * The panel shows the RAW text. This is the user reading their own screenshots, which is not an
//     egress event; redaction happens on the paths that leave the machine (the CLI, and anything an
//     agent reads). Keeping the two apart is the whole point of redacting at read rather than at
//     index time.
// ---------------------------------------------------------------------------------------------

const TEXT_SNIPPET_WIDTH = 320;

function selectText() {
  cancelPointerDrag();
  pushNavEntry(navEntry('text'));
  state.currentView = 'text';
  state.currentCategory = null;
  render();
  loadTextStatus();
}

async function loadTextStatus() {
  const root = state.library?.root;
  if (!root) return;
  try {
    state.textStatus = await window.categorizerAPI.getTextStatus(root);
  } catch (error) {
    console.error('Text status failed:', error);
  }
  renderSidebar();
  if (state.currentView === 'text') renderText();
  loadTopicStatus();
}

function wireTextPanel() {
  els.textSearchButton.addEventListener('click', () => runTextQuery());
  els.textQuery.addEventListener('keydown', event => {
    if (event.key === 'Enter') runTextQuery();
  });
  els.textFrom.addEventListener('change', () => runTextQuery());
  els.textTo.addEventListener('change', () => runTextQuery());
  els.textIncludeDupes.addEventListener('change', () => runTextQuery());
  els.textRequireAll.addEventListener('change', () => runTextQuery());
  els.textBucketHours.addEventListener('change', () => {
    // Topics are stored per width, so changing it changes how much is named.
    loadTopicStatus();
    if (state.textMode === 'buckets') runTextQuery();
  });
  els.textTopicsButton.addEventListener('click', () => generateTopics());
  els.textTopicsStop.addEventListener('click', () => stopTopics());

  els.textRangePreset.addEventListener('change', () => {
    const days = Number(els.textRangePreset.value || 0);
    if (!days) {
      els.textFrom.value = '';
      els.textTo.value = '';
    } else {
      const now = new Date();
      const start = new Date(now.getTime() - (days - 1) * 86400000);
      els.textFrom.value = isoDateInput(start);
      els.textTo.value = isoDateInput(now);
    }
    runTextQuery();
  });

  els.textModeImages.addEventListener('click', () => setTextMode('images'));
  els.textModeBuckets.addEventListener('click', () => setTextMode('buckets'));

  els.textRefreshButton.addEventListener('click', () => loadTextStatus());
  els.textRebuildButton.addEventListener('click', () => rebuildTextIndex());
  els.textActionCategory.addEventListener('change', () => {
    els.textCategorizeButton.disabled = !els.textActionCategory.value;
  });
  els.textCategorizeButton.addEventListener('click', () => categorizeTextResults());
  els.textCopyButton.addEventListener('click', () => copyTextResults());
}

function setTextMode(mode) {
  if (state.textMode === mode) return;
  state.textMode = mode;
  els.textModeImages.classList.toggle('active', mode === 'images');
  els.textModeBuckets.classList.toggle('active', mode === 'buckets');
  // Lazy, and only for the mode that asks a question about time. The log is a file read on the
  // machine, not in the library, so it costs nothing the image list should be paying for.
  if (mode === 'buckets') loadCaptureActivity();
  runTextQuery();
}

function isoDateInput(date) {
  const pad = value => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

// The index stores LOCAL civil seconds — the screenshot filename's own wall clock, with no timezone
// applied. So a date from the picker converts by reading its fields, never via `getTime()`, which
// would shift the whole range by the UTC offset and quietly drop a day at either end.
function civilSecondsFromInput(value, endOfDay) {
  if (!value) return null;
  const [year, month, day] = value.split('-').map(Number);
  if (!year || !month || !day) return null;
  const days = Math.floor(Date.UTC(year, month - 1, day) / 86400000);
  return days * 86400 + (endOfDay ? 86399 : 0);
}

function textQueryArgs() {
  return {
    query: els.textQuery.value || '',
    from: civilSecondsFromInput(els.textFrom.value, false),
    to: civilSecondsFromInput(els.textTo.value, true),
    folders: [],
    minChars: 0,
    includeDupes: els.textIncludeDupes.checked,
    requireAll: els.textRequireAll.checked,
    limit: 200,
  };
}

async function runTextQuery() {
  const root = state.library?.root;
  if (!root) return;
  const args = textQueryArgs();
  if (state.textMode === 'images' && !args.query.trim()) {
    state.textHits = null;
    state.textMatched = 0;
    state.textUnknownTerms = [];
    renderText();
    return;
  }

  // Every query carries a token; a slower earlier one landing later must not overwrite a newer
  // result. The search box fires on Enter rather than per keystroke, but the date pickers and the
  // toggles all fire too, and those overlap easily.
  const token = ++state.textQueryToken;
  state.textBusy = true;
  renderText();

  try {
    if (state.textMode === 'buckets') {
      const hours = Number(els.textBucketHours.value || 48);
      const buckets = await window.categorizerAPI.getTextTimeline(root, args, hours);
      if (token !== state.textQueryToken) return;
      state.textBuckets = buckets;
    } else {
      const result = await window.categorizerAPI.searchText(root, args, TEXT_SNIPPET_WIDTH);
      if (token !== state.textQueryToken) return;
      state.textHits = result.hits;
      state.textMatched = result.matched;
      state.textUnknownTerms = result.unknownTerms || [];
      if (result.phraseCapped) {
        showToast('Phrase checked against the top-ranked candidates only.');
      }
    }
    // A query may have built the index, so the status line beside it is now out of date.
    state.textStatus = await window.categorizerAPI.getTextStatus(root);
  } catch (error) {
    if (token === state.textQueryToken) showToast(errorText(error));
  } finally {
    if (token === state.textQueryToken) {
      state.textBusy = false;
      renderText();
      renderSidebar();
    }
  }
}

/// Tops the index up from the panel: OCR the in-scope images that have none, then rebuild.
///
/// Goes through the ordinary analysis queue rather than calling the command directly, so it inherits
/// everything that already works there — the interaction lock, the status line, the Stop button, and
/// the rescan on the way out. A second, parallel path would be a second set of those bugs.
async function extractPendingText() {
  if (!state.library) return;
  if (state.analyzing) {
    showToast('Another analysis pass is running.');
    return;
  }
  const status = state.textStatus;
  const pending = status?.pending || 0;
  if (!pending) return;

  const blocked = status?.blockedCategories || [];
  const reachable = status?.reachablePending ?? pending;
  const scope = (status?.categories || []).join('/') || 'in-scope';

  // Nothing is reachable because the category the index covers is switched off for analysis. Lifting
  // that is a real settings change, so it is asked for explicitly rather than folded into "extract".
  if (reachable === 0 && blocked.length) {
    const names = blocked.join(', ');
    const confirmed = window.confirm(
      `${names} ${blocked.length > 1 ? 'are' : 'is'} excluded from analysis, so extraction skips ` +
        `${blocked.length > 1 ? 'them' : 'it'} — that is why nothing runs.\n\n` +
        `Include ${names} in analysis and extract ${formatCount(pending)} images?\n\n` +
        'This also lets the other Analyze passes see these images again.'
    );
    if (!confirmed) return;
    try {
      for (const category of blocked) {
        await window.categorizerAPI.setCategoryAnalysisIncluded(state.library.root, category, true);
      }
      state.library = await window.categorizerAPI.scanLibrary(state.library.root);
      render();
    } catch (error) {
      showToast(errorText(error));
      return;
    }
  } else {
    const confirmed = window.confirm(
      `Extract text from ${formatCount(reachable)} ${scope} images?\n\n` +
        'This is OCR on this machine — no model, no network — but it takes a while at this count. ' +
        'You can stop it part way; everything done by then is kept.'
    );
    if (!confirmed) return;
  }

  state.analysisQueue = [{ type: 'ocr', force: false, indexedOnly: true }];
  setInteractionsLocked(true);
  await runNextInQueue();
}

async function rebuildTextIndex() {
  const root = state.library?.root;
  if (!root) return;
  els.textRebuildButton.disabled = true;
  try {
    const report = await window.categorizerAPI.buildTextIndex(root);
    showToast(`Indexed ${formatCount(report.docs)} documents in ${formatCount(report.elapsedMs)} ms.`);
    await loadTextStatus();
    if (state.textHits || state.textBuckets) runTextQuery();
  } catch (error) {
    showToast(errorText(error));
  } finally {
    els.textRebuildButton.disabled = false;
  }
}

async function selectTextDocument(hash) {
  const root = state.library?.root;
  if (!root) return;
  state.textSelectedHash = hash;
  state.textDetail = null;
  renderText();
  try {
    state.textDetail = await window.categorizerAPI.getImageText(root, hash);
  } catch (error) {
    showToast(errorText(error));
  }
  if (state.currentView === 'text') renderText();
}

function textResultHashes() {
  return (state.textHits || []).map(hit => hit.hash);
}

async function categorizeTextResults() {
  const root = state.library?.root;
  const category = els.textActionCategory.value;
  const hashes = textResultHashes();
  if (!root || !category || !hashes.length) return;

  // An image holds ONE category, so this overwrites whatever these were filed under — and since
  // the index only covers High Text, filing them elsewhere also drops them out of search. Both are
  // fine when meant and unpleasant when not, and there is no undo, so a whole result set is
  // confirmed. Same bar as deleting a category, which already asks.
  const confirmed = window.confirm(
    `Move ${hashes.length} images to "${category}"?\n\n` +
      'This replaces the category each one currently has. Anything moved out of the indexed ' +
      'categories also leaves the text index.'
  );
  if (!confirmed) return;

  els.textCategorizeButton.disabled = true;
  try {
    const changed = await window.categorizerAPI.categorizeImages(root, hashes, category);
    showToast(`${formatCount(changed)} of ${formatCount(hashes.length)} images moved to ${category}.`);
    await refreshLibrary();
    // Categories decide what is indexed, so moving images out of scope leaves the index describing
    // a library that no longer exists. Rebuilding here keeps the panel honest about its own results.
    await rebuildTextIndex();
  } catch (error) {
    showToast(errorText(error));
  } finally {
    els.textCategorizeButton.disabled = false;
  }
}

async function copyTextResults() {
  const hits = state.textHits || [];
  if (!hits.length) return;
  const lines = hits.map(hit => `## ${hit.at}  ${hit.path}\n${hit.snippet}`);
  try {
    await navigator.clipboard.writeText(lines.join('\n\n'));
    showToast(`Copied ${formatCount(hits.length)} results.`);
  } catch (error) {
    showToast(errorText(error));
  }
}

function renderText() {
  const status = state.textStatus;

  els.textStatusLine.textContent = state.textBusy ? 'Searching…' : textStatusSummary(status);

  // The reading pane has nothing to show in timeline mode — a bucket is a count and a keyword list,
  // not a document — so it gives its half back rather than sitting there saying "pick a result".
  els.textView.classList.toggle('buckets-mode', state.textMode === 'buckets');

  renderTextCoverage(status);
  renderCaptureActivity();
  renderTextTopicsBar();
  renderTextActions();

  if (state.textMode === 'buckets') renderTextBuckets();
  else renderTextHits();

  renderTextDetail();
}

function textStatusSummary(status) {
  if (!status) return '';
  if (!status.docs) return 'No index yet — search to build one.';
  const parts = [`${formatCount(status.docs)} documents`];
  if (status.exactDupes) parts.push(`${formatCount(status.exactDupes)} identical hidden`);
  if (status.nearDupes) parts.push(`${formatCount(status.nearDupes)} near demoted`);
  if (status.spanFrom) parts.push(`${status.spanFrom} → ${status.spanTo}`);
  return parts.join(' · ');
}

function renderTextCoverage(status) {
  els.textCoverage.innerHTML = '';
  if (!status) return;

  const line = document.createElement('div');
  line.className = 'text-coverage-line';

  const scope = document.createElement('span');
  scope.textContent =
    `${status.categories.join(', ')}: ${formatCount(status.extracted)} of ${formatCount(status.inScope)} extracted`;
  line.append(scope);

  if (status.pending) {
    // Still stated without urgency — a screenshot corpus is meant to have holes — but it is the one
    // number on this panel with an obvious next action, so it IS that action rather than a pointer
    // to one somewhere else.
    //
    // The catch, and the reason this is not just a button: the analysis scope is a SEPARATE axis
    // from what the index covers, and on a real library they disagree. Extraction honours
    // `excludedAnalysisCategories`, and High Text — the category the whole index is built from — is
    // routinely excluded there. Offering to extract 5,265 images and then silently doing nothing is
    // the failure this avoids, so a blocked count says what is blocking it and clicking it offers
    // to lift exactly that.
    const blockedCategories = status.blockedCategories || [];
    const blockedFolders = status.blockedFolders || [];
    const reachable = status.reachablePending ?? status.pending;

    const pending = document.createElement('button');
    pending.type = 'button';
    pending.className = 'text-coverage-pending';
    pending.disabled = state.analyzing;
    pending.addEventListener('click', () => extractPendingText());

    if (reachable === 0 && blockedCategories.length) {
      pending.classList.add('blocked');
      pending.textContent =
        `${formatCount(status.pending)} not extracted — ${blockedCategories.join(', ')} excluded from analysis`;
      pending.title =
        `Extraction skips ${blockedCategories.join(', ')} because ${blockedCategories.length > 1 ? 'they are' : 'it is'} ` +
        'switched off in the Categories list. Click to include and extract.';
    } else {
      pending.textContent = `Extract ${formatCount(reachable)} more`;
      pending.title =
        `Run OCR over the ${formatCount(reachable)} ${status.categories.join('/')} images that have ` +
        'no extracted text yet, then rebuild the index. Cancellable; also `icat extract`.';
    }
    line.append(pending);

    // Excluded FOLDERS are left as a statement, not an offer. A folder switched off means "don't
    // look in here at all", which is a broader intent than one category's text — turning it back on
    // would change what every other pass sees too.
    if (blockedFolders.length) {
      const folders = document.createElement('span');
      folders.className = 'text-coverage-note';
      folders.textContent = `${blockedFolders.join(', ')} excluded from analysis`;
      folders.title = 'Source folders switched off in the sidebar. Nothing here will extract them.';
      line.append(folders);
    }
  }

  if (status.staleReason) {
    const stale = document.createElement('span');
    stale.className = 'text-coverage-stale';
    stale.textContent = status.staleReason;
    line.append(stale);
  }

  els.textCoverage.append(line);
}

// ---------------------------------------------------------------------------------------------
// Capture activity — how many screenshots were TAKEN, from screenshot-tool's log
//
// This panel otherwise measures the library: files on disk, text extracted from them, buckets
// built over them. That is the right answer to "what can I categorize" and the wrong answer to
// "how much have I been capturing" — a month cleared out of the save folder is gone from every
// figure here, and it should not be gone from a record of the user's own habit.
//
// So the log is read as a SECOND, INDEPENDENT source and the two are never reconciled. The strip
// shows the act; the timeline beside it shows what survived. A gap between them is a cleanup, not
// a defect, and nothing here is allowed to call it missing data.
//
// ⚠ Every zero is qualified before it is drawn. The tool is a tray app that records only while it
// runs, and the log can be switched off in its own settings, so "0 screenshots" has three possible
// meanings and only one of them is about the user. `coverage.conclusive` is what separates them —
// see `capture_log.rs`.
// ---------------------------------------------------------------------------------------------

const CAPTURE_ACTIVITY_DAYS = 30;

/** Loaded once per app run, then reused: the log is on disk and slow-moving, and the timeline is
 *  re-rendered on every keystroke in the search box. */
async function loadCaptureActivity() {
  if (state.captureActivity !== undefined || state.captureActivityLoading) return;
  state.captureActivityLoading = true;
  try {
    state.captureActivity = await categorizerAPI.getCaptureActivity(CAPTURE_ACTIVITY_DAYS);
  } catch (error) {
    // A missing or unreadable log is a normal state, not an error worth a toast: most machines
    // will never have screenshot-tool on them. The strip simply stays hidden.
    console.warn('capture log unavailable', error);
    state.captureActivity = { blocked: 'not_installed' };
  } finally {
    state.captureActivityLoading = false;
  }
  if (state.textMode === 'buckets') renderCaptureActivity();
}

/** The one sentence that must be right: why there is nothing to show. Four different reasons and
 *  collapsing any two of them says something false about either the tool or the user. */
function captureBlockedText(blocked) {
  switch (blocked) {
    case 'logging_disabled':
      // A SETTING, not an outage. The tool may be capturing perfectly right now, and calling this
      // "not running" would be a false statement about software that is open.
      return 'Capture logging is switched off in Screenshot Tool\'s settings.';
    case 'never_logged':
      return 'Screenshot Tool has not logged anything yet.';
    default:
      return null; // not installed — say nothing at all rather than explain an absent app
  }
}

/** Minutes as the coarsest unit that still says something: a log four hours old should not be
 *  described in minutes, and one three weeks old should not be described in hours. */
function humanizeMinutes(minutes) {
  if (minutes < 90) return `${Math.max(1, Math.round(minutes))} min`;
  if (minutes < 2880) return `${Math.round(minutes / 60)} hours`;
  return `${Math.round(minutes / 1440)} days`;
}

function renderCaptureActivity() {
  const el = els.captureActivity;
  el.innerHTML = '';

  const data = state.captureActivity;
  const show = state.textMode === 'buckets' && !!data;
  el.classList.toggle('hidden', !show);
  if (!show) return;

  if (data.blocked) {
    const text = captureBlockedText(data.blocked);
    if (!text) {
      el.classList.add('hidden');
      return;
    }
    const note = document.createElement('span');
    note.className = 'capture-note';
    note.textContent = text;
    el.append(note);
    return;
  }

  const days = Object.entries(data.by_day || {});
  const peak = days.reduce((max, [, n]) => Math.max(max, n), 0);

  const label = document.createElement('span');
  label.className = 'capture-label';
  label.textContent = 'Screenshots taken';
  label.title =
    'From Screenshot Tool\'s own log, not from this library. It records the act, so it does not ' +
    'fall when you delete or move pictures — the two are meant to disagree.';
  el.append(label);

  // The bars. One per day in the window, including days with none: a missing bar and a zero bar
  // are different claims, and only drawing the days that had captures would quietly hide every
  // quiet day.
  const strip = document.createElement('div');
  strip.className = 'capture-strip';
  for (const [day, count] of days) {
    const bar = document.createElement('div');
    bar.className = 'capture-bar';
    bar.style.setProperty('--h', peak > 0 ? `${Math.max(4, (count / peak) * 100)}%` : '0%');
    bar.title = `${day} — ${formatCount(count)} screenshot${count === 1 ? '' : 's'}`;
    strip.append(bar);
  }
  el.append(strip);

  const stats = document.createElement('span');
  stats.className = 'capture-stats';
  const parts = [`${formatCount(data.total)} in ${data.days} days`];
  if (data.median_per_active_day != null) {
    // The baseline is stated with the total and never as a verdict. "A lot" and "a little" only
    // mean anything against this user's own habit, and the app has no business ranking their day.
    parts.push(`typically ${formatCount(data.median_per_active_day)} on a day you capture`);
  }
  if (data.peak_hour != null) parts.push(`busiest around ${String(data.peak_hour).padStart(2, '0')}:00`);
  stats.textContent = parts.join(' · ');
  el.append(stats);

  // ⚠ The qualifier on every count. Zero screenshots with the tool closed is not a measurement,
  // and a running tool that has not heartbeat into this window yet is not evidence of anything.
  if (data.coverage && data.coverage.coverage_pct < 50) {
    const cov = document.createElement('span');
    cov.className = 'capture-note';
    if (!data.coverage.conclusive) {
      cov.textContent = 'Coverage not yet established — treat low counts as unknown, not as zero.';
    } else {
      // Named against the log's own lifetime when it is younger than the window, so a two-day-old
      // log does not read as twenty-eight days of a closed app.
      const period = data.coverage.truncated_to_log
        ? `the log's ${humanizeMinutes(data.coverage.window_min)} of history`
        : 'this window';
      cov.textContent =
        `Screenshot Tool was open for ${data.coverage.coverage_pct}% of ${period}, so this is a ` +
        'sample of when it was running, not of the period.';
    }
    el.append(cov);
  }

  // Stated plainly and only when it exists, so the difference against the library has a named
  // cause rather than looking like a scan that lost files.
  if (data.failed > 0) {
    const failed = document.createElement('span');
    failed.className = 'capture-note';
    failed.textContent = `${formatCount(data.failed)} failed to save a file`;
    failed.title =
      'The screenshot was taken but the PNG never landed, so no file for it exists in any folder. ' +
      'Expected to be missing here — not a fault in this library.';
    el.append(failed);
  }

  if (data.unknown_schema && data.unknown_schema.length) {
    const schema = document.createElement('span');
    schema.className = 'capture-note';
    schema.textContent = `${data.unknown_schema.length} log file(s) in a newer format — not counted`;
    schema.title =
      `${data.unknown_schema.join(', ')} use a schema this app does not parse, so those months are ` +
      'missing from every figure here. Not a quiet month — an unread one.';
    el.append(schema);
  }
}

function renderTextActions() {
  const hits = state.textHits || [];
  const show = state.textMode === 'images' && hits.length > 0;
  els.textActions.classList.toggle('hidden', !show);
  if (!show) return;

  els.textActionsLabel.textContent = `${formatCount(hits.length)} results`;

  const categories = (state.library?.categories || []).map(category => category.name);
  const previous = els.textActionCategory.value;
  els.textActionCategory.innerHTML = '';

  // A placeholder first, and the button stays dead until something is chosen. Without it the
  // select defaults to whatever category happens to sort first — which here is `Explicit` — so a
  // single click on a 200-result set would file two hundred screenshots under it.
  const placeholder = document.createElement('option');
  placeholder.value = '';
  placeholder.textContent = 'Choose a category…';
  els.textActionCategory.append(placeholder);

  for (const name of categories) {
    const option = document.createElement('option');
    option.value = name;
    option.textContent = name;
    els.textActionCategory.append(option);
  }
  els.textActionCategory.value = categories.includes(previous) ? previous : '';
  els.textCategorizeButton.disabled = !els.textActionCategory.value;
}

function renderTextHits() {
  els.textResults.innerHTML = '';
  const hits = state.textHits;

  if (state.textBusy && !hits) {
    els.textResults.append(textEmpty('Searching…'));
    return;
  }
  if (!hits) {
    els.textResults.append(
      textEmpty('Type a query and press Enter. Quote a "phrase" to require it exactly.')
    );
    return;
  }
  if (!hits.length) {
    const unknown = state.textUnknownTerms || [];
    els.textResults.append(
      textEmpty(
        unknown.length
          ? `Nothing found. These were never on screen: ${unknown.join(', ')}`
          : 'Nothing matched in this range.'
      )
    );
    return;
  }

  const header = document.createElement('div');
  header.className = 'text-results-head';
  header.textContent = `${formatCount(state.textMatched)} matched · showing ${formatCount(hits.length)}`;
  if ((state.textUnknownTerms || []).length) {
    const unknown = document.createElement('span');
    unknown.className = 'text-unknown-terms';
    unknown.textContent = `never on screen: ${state.textUnknownTerms.join(', ')}`;
    header.append(unknown);
  }
  els.textResults.append(header);

  for (const hit of hits) {
    els.textResults.append(renderTextHit(hit));
  }
}

function renderTextHit(hit) {
  const row = document.createElement('button');
  row.type = 'button';
  row.className = 'text-hit';
  row.classList.toggle('selected', hit.hash === state.textSelectedHash);
  // A near-duplicate is dimmed rather than hidden: it ranks below first-hand hits but stays
  // readable, because the lines it added are often exactly what was being looked for.
  row.classList.toggle('near-dupe', hit.rank === 2);

  const head = document.createElement('div');
  head.className = 'text-hit-head';

  const when = document.createElement('span');
  when.className = 'text-hit-when';
  when.textContent = hit.at;
  head.append(when);

  const path = document.createElement('span');
  path.className = 'text-hit-path';
  path.textContent = hit.path;
  path.title = hit.path;
  head.append(path);

  for (const badge of textHitBadges(hit)) {
    const element = document.createElement('span');
    element.className = 'text-badge';
    element.textContent = badge;
    head.append(element);
  }

  row.append(head);

  if (hit.terms?.length) {
    const terms = document.createElement('div');
    terms.className = 'text-hit-terms';
    terms.textContent = hit.terms.join(' · ');
    row.append(terms);
  }

  const snippet = document.createElement('div');
  snippet.className = 'text-hit-snippet';
  snippet.textContent = hit.snippet;
  row.append(snippet);

  row.addEventListener('click', () => selectTextDocument(hit.hash));
  return row;
}

function textHitBadges(hit) {
  const badges = [];
  if (hit.rank === 2) badges.push('near-dupe');
  if (hit.exactDupes) badges.push(`+${hit.exactDupes} identical`);
  if (hit.nearDupes) badges.push(`+${hit.nearDupes} near`);
  return badges;
}

function renderTextBuckets() {
  els.textResults.innerHTML = '';
  const buckets = state.textBuckets;
  if (state.textBusy && !buckets) {
    els.textResults.append(textEmpty('Building the timeline…'));
    return;
  }
  if (!buckets || !buckets.length) {
    els.textResults.append(textEmpty('No extracted text in this range.'));
    return;
  }

  const busiest = buckets.reduce((most, bucket) => Math.max(most, bucket.images), 1);

  for (const bucket of buckets) {
    const row = document.createElement('div');
    row.className = 'text-bucket';

    const head = document.createElement('div');
    head.className = 'text-bucket-head';

    const label = document.createElement('span');
    label.className = 'text-bucket-id';
    label.textContent = bucket.id;
    head.append(label);

    const count = document.createElement('span');
    count.className = 'text-bucket-count';
    // A date range can cut through the middle of a bucket. The count then describes the part in
    // range while the topics under it describe the whole bucket, so the two are said apart rather
    // than left to look like one number.
    count.textContent = bucket.partial
      ? `${formatCount(bucket.images)} of ${formatCount(bucket.members)} images`
      : `${formatCount(bucket.images)} images`;
    if (bucket.partial) {
      count.title = 'Your date range covers part of this bucket. The topics below describe the whole bucket.';
    }
    head.append(count);

    if (bucket.exactDupes || bucket.nearDupes) {
      const dupes = document.createElement('span');
      dupes.className = 'text-badge';
      dupes.textContent = `${bucket.exactDupes} identical, ${bucket.nearDupes} near`;
      head.append(dupes);
    }
    row.append(head);

    const bar = document.createElement('div');
    bar.className = 'text-bucket-bar';
    const fill = document.createElement('div');
    fill.className = 'text-bucket-fill';
    fill.style.width = `${Math.max(2, Math.round((bucket.images / busiest) * 100))}%`;
    bar.append(fill);
    row.append(bar);

    // Topics first when they exist — they answer "what was this about", which the statistical
    // terms below only supply the raw material for. Both are shown: a topic phrase smooths away the
    // exact spellings (identifiers, error strings) that make a term searchable.
    if (bucket.topics?.length) {
      const topics = document.createElement('div');
      topics.className = 'text-bucket-topics';
      topics.textContent = bucket.topics.join(' · ');
      if (bucket.topicsStale) {
        const stale = document.createElement('span');
        stale.className = 'text-badge';
        stale.textContent = 'stale';
        stale.title = 'These names were written for a different set of images than this bucket now holds.';
        topics.append(' ', stale);
      }
      row.append(topics);
    }

    if (bucket.notable?.length) {
      const notable = document.createElement('div');
      notable.className = 'text-bucket-notable';
      notable.textContent = bucket.notable.join(' · ');
      row.append(notable);
    }

    if (bucket.terms?.length) {
      const terms = document.createElement('div');
      terms.className = 'text-bucket-terms';
      terms.textContent = bucket.terms.join(' · ');
      row.append(terms);
    }

    // Clicking a bucket narrows the date range to it and drops back to the image list — the
    // timeline's job is to be a way in, not a destination.
    row.addEventListener('click', () => {
      els.textFrom.value = isoDateInput(new Date(bucket.start * 1000));
      els.textTo.value = isoDateInput(new Date(bucket.end * 1000));
      els.textRangePreset.value = '';
      setTextMode('images');
    });
    els.textResults.append(row);
  }
}

function renderTextDetail() {
  els.textDetail.innerHTML = '';
  const detail = state.textDetail;

  if (!state.textSelectedHash) {
    els.textDetail.append(textEmpty('Pick a result to read its full text.'));
    return;
  }
  if (!detail) {
    els.textDetail.append(textEmpty('Loading…'));
    return;
  }

  const head = document.createElement('div');
  head.className = 'text-detail-head';

  const title = document.createElement('div');
  title.className = 'text-detail-title';
  title.textContent = detail.at;
  head.append(title);

  const path = document.createElement('div');
  path.className = 'text-detail-path';
  path.textContent = detail.path;
  head.append(path);

  const meta = document.createElement('div');
  meta.className = 'text-detail-meta';
  meta.textContent = `${detail.category} · ${formatCount(detail.chars)} chars`;
  head.append(meta);

  const actions = document.createElement('div');
  actions.className = 'text-detail-actions';

  const copy = document.createElement('button');
  copy.type = 'button';
  copy.className = 'button secondary';
  copy.textContent = 'Copy text';
  copy.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(detail.text);
      showToast('Text copied.');
    } catch (error) {
      showToast(errorText(error));
    }
  });
  actions.append(copy);

  const reveal = document.createElement('button');
  reveal.type = 'button';
  reveal.className = 'button secondary';
  reveal.textContent = 'Show image';
  reveal.addEventListener('click', async () => {
    const image = (state.library?.images || []).find(item => item.hash === detail.hash);
    if (!image) {
      showToast('That image is not in the current scan.');
      return;
    }
    try {
      await window.categorizerAPI.openImage(image.path);
    } catch (error) {
      showToast(errorText(error));
    }
  });
  actions.append(reveal);
  head.append(actions);
  els.textDetail.append(head);

  if (detail.terms?.length) {
    const terms = document.createElement('div');
    terms.className = 'text-detail-terms';
    terms.textContent = detail.terms.join(' · ');
    els.textDetail.append(terms);
  }

  const body = document.createElement('pre');
  body.className = 'text-detail-body';
  body.textContent = detail.text;
  els.textDetail.append(body);

  // Group members contribute only their NEW lines. A near-duplicate is usually the same screen
  // scrolled a little, and reprinting the whole thing would bury the two lines that differ.
  for (const member of detail.members || []) {
    const block = document.createElement('div');
    block.className = 'text-detail-member';

    const kind = member.rank === 1 ? 'identical copy' : 'near copy';
    const memberHead = document.createElement('div');
    memberHead.className = 'text-detail-member-head';
    memberHead.textContent = member.novelLines.length
      ? `${member.at} · ${kind} · added ${member.novelLines.length} lines`
      : `${member.at} · ${kind} · nothing new`;
    block.append(memberHead);

    if (member.novelLines.length) {
      const lines = document.createElement('pre');
      lines.className = 'text-detail-member-lines';
      lines.textContent = member.novelLines.join('\n');
      block.append(lines);
    }
    els.textDetail.append(block);
  }
}

function textEmpty(message) {
  const element = document.createElement('div');
  element.className = 'text-empty';
  element.textContent = message;
  return element;
}

// ---------------------------------------------------------------------------------------------
// Topic layer (phase 2)
//
// Statistical keywords already label every bucket for free. What they cannot do is see that
// `webview2`, `cdp` and `9400` are one subject — that is a synthesis job, and it is why this makes
// one local-model call per BUCKET rather than per image: 44 calls for this whole library instead of
// ~8,800. It never starts on its own; the button is the only trigger in the UI, `icat topics` the
// only one outside it.
// ---------------------------------------------------------------------------------------------

async function loadTopicStatus() {
  const root = state.library?.root;
  if (!root) return;
  try {
    state.topicStatus = await window.categorizerAPI.getTopicStatus(root, currentBucketHours());
  } catch (error) {
    // A library with no index yet is the normal case here, not something to surface.
    state.topicStatus = null;
  }
  if (state.currentView === 'text') renderTextTopicsBar();
}

function currentBucketHours() {
  return Number(els.textBucketHours.value || 48);
}

async function installTopicListeners() {
  await window.categorizerAPI.onTopicsProgress(payload => {
    state.topicRun = payload;
    renderTextTopicsBar();
  });
  await window.categorizerAPI.onTopicsFinished(payload => {
    state.topicRun = null;
    state.topicMessage = payload.message || null;
    if (payload.status === 'error') showToast(payload.message || 'Naming buckets failed.');
    else if (payload.message) showToast(payload.message);
    renderTextTopicsBar();
    // The timeline is what the run was for, so it repaints with the names in place.
    loadTopicStatus();
    if (state.currentView === 'text' && state.textMode === 'buckets') runTextQuery();
  });
}

async function generateTopics() {
  const root = state.library?.root;
  if (!root) return;
  const hours = currentBucketHours();
  const pending = state.topicStatus
    ? state.topicStatus.bucketsTotal - state.topicStatus.bucketsWithTopics + state.topicStatus.bucketsStale
    : null;

  // One model call per bucket, and the user is the one paying the wall-clock. Say how many before
  // starting rather than after — the count is the whole decision.
  const confirmed = window.confirm(
    pending
      ? `Ask the local model to name ${pending} bucket${pending === 1 ? '' : 's'} at ${hours}h?\n\n` +
        'One call each, text only. You can stop part way — finished buckets are kept.'
      : `Re-name every bucket at ${hours}h?`
  );
  if (!confirmed) return;

  state.topicMessage = null;
  state.topicRun = { processed: 0, total: pending || 0, currentBucket: '', topics: [] };
  renderTextTopicsBar();
  try {
    await window.categorizerAPI.generateTopics(root, hours, false);
  } catch (error) {
    state.topicRun = null;
    showToast(errorText(error));
    renderTextTopicsBar();
  }
}

async function stopTopics() {
  try {
    await window.categorizerAPI.cancelTopics();
  } catch (error) {
    showToast(errorText(error));
  }
}

function renderTextTopicsBar() {
  const running = !!state.topicRun;
  // The button belongs to the timeline: naming a span of time is meaningless for a result list.
  const show = state.textMode === 'buckets';
  els.textTopicsButton.classList.toggle('hidden', !show || running);
  els.textTopicsStop.classList.toggle('hidden', !show || !running);

  const status = state.topicStatus;
  if (show && status && status.bucketsTotal) {
    const parts = [`${formatCount(status.bucketsWithTopics)}/${formatCount(status.bucketsTotal)} named`];
    if (status.bucketsStale) parts.push(`${formatCount(status.bucketsStale)} stale`);
    els.textTopicsButton.textContent =
      status.bucketsWithTopics === 0 ? 'Name buckets…' : `Name buckets… (${parts.join(', ')})`;
  } else {
    els.textTopicsButton.textContent = 'Name buckets…';
  }

  const line = state.topicRun
    ? `${formatCount(state.topicRun.processed)}/${formatCount(state.topicRun.total)} · ${state.topicRun.currentBucket}` +
      (state.topicRun.topics?.length ? ` — ${state.topicRun.topics.join(' · ')}` : '')
    : state.topicMessage;

  els.textTopicsProgress.classList.toggle('hidden', !show || !line);
  els.textTopicsProgress.textContent = line || '';
}

init();
