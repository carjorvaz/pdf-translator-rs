/**
 * htmx preload extension
 * @see https://htmx.org/extensions/preload/
 *
 * This adds the "preload" extension to htmx. The extension will
 * preload the targets of elements with "preload" attribute if:
 * - they also have `href`, `hx-get` or `data-hx-get` attributes
 * - they are radio buttons, checkboxes, select elements and submit
 *   buttons of forms with `method="get"` or `hx-get` attributes
 * The extension relies on browser cache and for it to work
 * server response must include `Cache-Control` header
 * e.g. `Cache-Control: private, max-age=60`.
 */
(function() {
  htmx.defineExtension('preload', {
    onEvent: function(name, event) {
      if (name === 'htmx:afterProcessNode') {
        const parent = event.target || event.detail.elt;
        const preloadNodes = [
          ...parent.hasAttribute("preload") ? [parent] : [],
          ...parent.querySelectorAll("[preload]")]
        preloadNodes.forEach(function(node) {
          init(node)
          node.querySelectorAll('[href],[hx-get],[data-hx-get]').forEach(init)
        })
        return
      }

      if (name === 'htmx:beforeRequest') {
        const requestHeaders = event.detail.requestConfig.headers
        if (!("HX-Preloaded" in requestHeaders
              && requestHeaders["HX-Preloaded"] === "true")) {
          return
        }

        event.preventDefault()
        const xhr = event.detail.xhr
        xhr.onload = function() {
          processResponse(event.detail.elt, xhr.responseText)
        }
        xhr.onerror = null
        xhr.onabort = null
        xhr.ontimeout = null
        xhr.send()
      }
    }
  })

  function init(node) {
    if (node.preloadState !== undefined) {
      return
    }

    if (!isValidNodeForPreloading(node)) {
      return
    }

    if (node instanceof HTMLFormElement) {
      const form = node
      if (!((form.hasAttribute('method') && form.method === 'get')
        || form.hasAttribute('hx-get') || form.hasAttribute('hx-data-get'))) {
        return
      }
      for (let i = 0; i < form.elements.length; i++) {
        const element = form.elements.item(i);
        init(element);
        if ("labels" in element) {
          element.labels.forEach(init);
        }
      }
      return
    }

    let preloadAttr = getClosestAttribute(node, 'preload');
    node.preloadAlways = preloadAttr && preloadAttr.includes('always');
    if (node.preloadAlways) {
      preloadAttr = preloadAttr.replace('always', '').trim();
    }
    let triggerEventName = preloadAttr || 'mousedown';

    const needsTimeout = triggerEventName === 'mouseover'
    node.addEventListener(triggerEventName, getEventHandler(node, needsTimeout), {passive: true})

    if (triggerEventName === 'mousedown' || triggerEventName === 'mouseover') {
      node.addEventListener('touchstart', getEventHandler(node), {passive: true})
    }

    if (triggerEventName === 'mouseover') {
      node.addEventListener('mouseout', function(evt) {
        if ((evt.target === node) && (node.preloadState === 'TIMEOUT')) {
          node.preloadState = 'READY'
        }
      }, {passive: true})
    }

    node.preloadState = 'READY'
    htmx.trigger(node, 'preload:init')
  }

  function getEventHandler(node, needsTimeout = false) {
    return function() {
      if (node.preloadState !== 'READY') {
        return
      }

      if (needsTimeout) {
        node.preloadState = 'TIMEOUT'
        const timeoutMs = 100
        window.setTimeout(function() {
          if (node.preloadState === 'TIMEOUT') {
            node.preloadState = 'READY'
            load(node)
          }
        }, timeoutMs)
        return
      }

      load(node)
    }
  }

  function load(node) {
    if (node.preloadState !== 'READY') {
      return
    }
    node.preloadState = 'LOADING'

    const hxGet = node.getAttribute('hx-get') || node.getAttribute('data-hx-get')
    if (hxGet) {
      sendHxGetRequest(hxGet, node);
      return
    }

    const hxBoost = getClosestAttribute(node, "hx-boost") === "true"
    if (node.hasAttribute('href')) {
      const url = node.getAttribute('href');
      if (hxBoost) {
        sendHxGetRequest(url, node);
      } else {
        sendXmlGetRequest(url, node);
      }
      return
    }

    if (isPreloadableFormElement(node)) {
      const url = node.form.getAttribute('action')
                  || node.form.getAttribute('hx-get')
                  || node.form.getAttribute('data-hx-get');
      const formData = htmx.values(node.form);
      const isStandardForm = !(node.form.getAttribute('hx-get')
                              || node.form.getAttribute('data-hx-get')
                              || hxBoost);
      const sendGetRequest = isStandardForm ? sendXmlGetRequest : sendHxGetRequest

      if (node.type === 'submit') {
        sendGetRequest(url, node.form, formData)
        return
      }

      const inputName = node.name || node.control.name;
      if (node.tagName === 'SELECT') {
        Array.from(node.options).forEach(option => {
          if (option.selected) return;
          formData.set(inputName, option.value);
          const formDataOrdered = forceFormDataInOrder(node.form, formData);
          sendGetRequest(url, node.form, formDataOrdered)
        });
        return
      }

      const inputType = node.getAttribute("type") || node.control.getAttribute("type");
      const nodeValue = node.value || node.control?.value;
      if (inputType === 'radio') {
        formData.set(inputName, nodeValue);
      } else if (inputType === 'checkbox'){
        const inputValues = formData.getAll(inputName);
        if (inputValues.includes(nodeValue)) {
          formData[inputName] = inputValues.filter(value => value !== nodeValue);
        } else {
          formData.append(inputName, nodeValue);
        }
      }
      const formDataOrdered = forceFormDataInOrder(node.form, formData);
      sendGetRequest(url, node.form, formDataOrdered)
      return
    }
  }

  function forceFormDataInOrder(form, formData) {
    const formElements = form.elements;
    const orderedFormData = new FormData();
    for(let i = 0; i < formElements.length; i++) {
      const element = formElements.item(i);
      if (formData.has(element.name) && element.tagName === 'SELECT') {
        orderedFormData.append(
          element.name, formData.get(element.name));
        continue;
      }
      if (formData.has(element.name) && formData.getAll(element.name)
        .includes(element.value)) {
        orderedFormData.append(element.name, element.value);
      }
    }
    return orderedFormData;
  }

  function sendHxGetRequest(url, sourceNode, formData = undefined) {
    htmx.ajax('GET', url, {
      source: sourceNode,
      values: formData,
      headers: {"HX-Preloaded": "true"}
    });
  }

  function sendXmlGetRequest(url, sourceNode, formData = undefined) {
    const xhr = new XMLHttpRequest()
    if (formData) {
      url += '?' + new URLSearchParams(formData.entries()).toString()
    }
    xhr.open('GET', url);
    xhr.setRequestHeader("HX-Preloaded", "true")
    xhr.onload = function() { processResponse(sourceNode, xhr.responseText) }
    xhr.send()
  }

  function processResponse(node, responseText) {
    node.preloadState = node.preloadAlways ? 'READY' : 'DONE'

    if (getClosestAttribute(node, 'preload-images') === 'true') {
      document.createElement('div').innerHTML = responseText
    }
  }

  function getClosestAttribute(node, attribute) {
    if (node == undefined) { return undefined }
    return node.getAttribute(attribute)
      || node.getAttribute('data-' + attribute)
      || getClosestAttribute(node.parentElement, attribute)
  }

  function isValidNodeForPreloading(node) {
    const getReqAttrs = ['href', 'hx-get', 'data-hx-get'];
    const includesGetRequest = node => getReqAttrs.some(a => node.hasAttribute(a))
                                        || node.method === 'get';
    const isPreloadableGetFormElement = node.form instanceof HTMLFormElement
                                        && includesGetRequest(node.form)
                                        && isPreloadableFormElement(node)
    if (!includesGetRequest(node) && !isPreloadableGetFormElement) {
      return false
    }

    if (node instanceof HTMLInputElement && node.closest('label')) {
      return false
    }

    return true
  }

  function isPreloadableFormElement(node) {
    if (node instanceof HTMLInputElement || node instanceof HTMLButtonElement) {
      const type = node.getAttribute('type');
      return ['checkbox', 'radio', 'submit'].includes(type);
    }
    if (node instanceof HTMLLabelElement) {
      return node.control && isPreloadableFormElement(node.control);
    }
    return node instanceof HTMLSelectElement;
  }
})()

/* Application upload and auto-translation UI event handling. */
;(function() {
  function setUploadProgress(form, percent, text) {
    const bar = form.querySelector('#upload-progress-bar')
    const fill = form.querySelector('#upload-progress-fill')
    const label = form.querySelector('#upload-progress-text')
    fill.style.width = `${percent}%`
    bar.setAttribute('aria-valuenow', String(percent))
    bar.setAttribute('aria-valuetext', text)
    label.textContent = text
  }

  function uploadForm(event) {
    const element = event.detail && event.detail.elt
    return element instanceof Element ? element.closest('form.upload-area') : null
  }

  function beginUpload(form) {
    form.classList.add('uploading')
    form.classList.remove('processing')
    form.setAttribute('aria-busy', 'true')
    const error = form.querySelector('#upload-error')
    error.textContent = ''
    error.hidden = true
    setUploadProgress(form, 0, 'Starting upload')
  }

  function finishUpload(form) {
    form.classList.add('processing')
    setUploadProgress(form, 100, 'Upload complete. Processing PDF...')
  }

  function failUpload(form) {
    form.classList.remove('uploading', 'processing')
    form.removeAttribute('aria-busy')
    const input = form.querySelector('#file-input')
    input.disabled = false
    input.value = ''
    setUploadProgress(form, 0, 'Upload failed')
    const error = form.querySelector('#upload-error')
    error.textContent = 'Upload failed. Choose a PDF and try again.'
    error.hidden = false
  }

  function autoTranslateContainer(event) {
    const element = event.detail && event.detail.elt
    if (!(element instanceof Element)) return null
    const container = element.closest('.placeholder')
    return container && container.querySelector('#auto-translate-status') ? container : null
  }

  function beginAutoTranslate(container) {
    container.querySelector('#auto-translate-status').textContent = 'Translating...'
    container.querySelector('#auto-translate-retry').hidden = true
  }

  function failAutoTranslate(container) {
    container.querySelector('#auto-translate-status').textContent =
      'Translation failed. Select Try again to retry.'
    container.querySelector('#auto-translate-retry').hidden = false
  }

  function bindUpload(root) {
    const form = root.matches && root.matches('form.upload-area')
      ? root
      : root.querySelector && root.querySelector('form.upload-area')
    if (!form || form.dataset.uploadInitialized === 'true') return
    const input = form.querySelector('#file-input')
    if (!input) return
    form.dataset.uploadInitialized = 'true'

    input.addEventListener('change', function() {
      if (input.files.length > 0) form.requestSubmit()
    })
    for (const eventName of ['dragenter', 'dragover']) {
      form.addEventListener(eventName, function(event) {
        event.preventDefault()
        form.classList.add('dragover')
      })
    }
    form.addEventListener('dragleave', function() {
      form.classList.remove('dragover')
    })
    form.addEventListener('drop', function(event) {
      event.preventDefault()
      form.classList.remove('dragover')
      if (event.dataTransfer.files.length === 0) return
      input.files = event.dataTransfer.files
      input.dispatchEvent(new Event('change', { bubbles: true }))
    })
  }

  function initialize() {
    document.body.addEventListener('htmx:beforeRequest', function(event) {
      const form = uploadForm(event)
      if (form) beginUpload(form)
      const autoTranslate = autoTranslateContainer(event)
      if (autoTranslate) beginAutoTranslate(autoTranslate)
    })

    document.body.addEventListener('htmx:xhr:progress', function(event) {
      const form = uploadForm(event)
      if (!form || !event.detail.total) return
      const percent = Math.round(event.detail.loaded / event.detail.total * 100)
      setUploadProgress(form, percent, `${percent}% uploaded`)
    })

    document.body.addEventListener('htmx:afterRequest', function(event) {
      const form = uploadForm(event)
      if (form) {
        if (event.detail.successful) finishUpload(form)
        else failUpload(form)
      }
      if (!event.detail.successful) {
        const autoTranslate = autoTranslateContainer(event)
        if (autoTranslate) failAutoTranslate(autoTranslate)
      }
    })

    for (const eventName of ['htmx:responseError', 'htmx:sendError']) {
      document.body.addEventListener(eventName, function(event) {
        const form = uploadForm(event)
        if (form) failUpload(form)
        const autoTranslate = autoTranslateContainer(event)
        if (autoTranslate) failAutoTranslate(autoTranslate)
      })
    }

    document.body.addEventListener('htmx:load', function(event) {
      bindUpload(event.target)
    })
    bindUpload(document)
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initialize, { once: true })
  } else {
    initialize()
  }
})()
