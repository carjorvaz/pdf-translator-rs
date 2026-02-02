/**
 * PDF Translator Web - Client-side JavaScript
 * Only what can't be done with HTMX:
 * - Drag-drop file handling (HTML5 drag events)
 * - Scroll sync between panels (coordinate scroll positions)
 * - View toggle (CSS class toggle for layout)
 */

// Drag-drop file handling (can't be done purely in HTMX)
document.addEventListener('DOMContentLoaded', function() {
    const uploadArea = document.getElementById('upload-area');
    const fileInput = document.getElementById('file-input');
    const uploadForm = document.getElementById('upload-form');

    if (uploadArea && fileInput && uploadForm) {
        uploadArea.onclick = () => fileInput.click();
        uploadArea.ondragover = e => { e.preventDefault(); uploadArea.classList.add('dragover'); };
        uploadArea.ondragleave = () => uploadArea.classList.remove('dragover');
        uploadArea.ondrop = e => {
            e.preventDefault();
            uploadArea.classList.remove('dragover');
            if (e.dataTransfer.files[0]) {
                fileInput.files = e.dataTransfer.files;
                htmx.trigger(uploadForm, 'submit');
            }
        };
        fileInput.onchange = () => {
            if (fileInput.files[0]) htmx.trigger(uploadForm, 'submit');
        };
    }
});

// Upload progress tracking
document.body.addEventListener('htmx:xhr:progress', function(e) {
    if (e.detail.loaded && e.detail.total) {
        const percent = Math.round((e.detail.loaded / e.detail.total) * 100);
        const uploadArea = document.getElementById('upload-area');
        const h2 = uploadArea?.querySelector('h2');
        if (h2 && percent < 100) {
            h2.textContent = `Uploading... ${percent}%`;
        } else if (h2 && percent === 100) {
            h2.textContent = 'Processing PDF...';
        }
    }
});

// Scroll sync between panels (needs to work after HTMX swaps)
document.body.addEventListener('htmx:afterSwap', function(e) {
    setupScrollSync();
    setupViewToggle();
});

function setupScrollSync() {
    const origPanel = document.getElementById('original-panel');
    const transPanel = document.getElementById('translated-panel');
    if (!origPanel || !transPanel) return;

    let ticking = false;
    let lastSource = null;

    function sync(source, target) {
        return function() {
            // Prevent feedback loops: ignore if another panel initiated scroll
            if (lastSource && lastSource !== source) return;
            lastSource = source;
            if (!ticking) {
                requestAnimationFrame(() => {
                    target.scrollTop = source.scrollTop;
                    ticking = false;
                    lastSource = null;
                });
                ticking = true;
            }
        };
    }

    origPanel.addEventListener('scroll', sync(origPanel, transPanel), { passive: true });
    transPanel.addEventListener('scroll', sync(transPanel, origPanel), { passive: true });
}

// View toggle (CSS class toggle)
function setupViewToggle() {
    const viewBoth = document.getElementById('view-both');
    const viewTranslated = document.getElementById('view-translated');
    const viewer = document.getElementById('viewer');

    if (viewBoth && viewTranslated && viewer) {
        viewBoth.onclick = function() {
            viewer.classList.remove('single');
            this.classList.add('active');
            viewTranslated.classList.remove('active');
        };
        viewTranslated.onclick = function() {
            viewer.classList.add('single');
            this.classList.add('active');
            viewBoth.classList.remove('active');
        };
    }
}

// Initial setup
setupScrollSync();
setupViewToggle();
