// EffectCraft Transfer: the plug-in's main thread. The serializer above (serialize.js) turns the
// selection into the payload; the panel (ui.html) pairs with EffectCraft and posts it to the
// Figma bridge EffectCraft runs on 127.0.0.1:7789.

figma.showUI(__html__, { width: 300, height: 236, themeColors: true });

figma.ui.onmessage = async function (msg) {
  if (msg.type === 'collect') {
    try {
      var payload = await effectcraftSerialize({ images: 'both',
        onProgress: function (text) { figma.ui.postMessage({ type: 'progress', text: text }); }
      });
      // The panel posts nodes and embedded images; the rest is bookkeeping.
      figma.ui.postMessage({ type: 'payload', payload: { source: payload.source, fileKey: payload.fileKey, nodes: payload.nodes, images: payload.images }, target: msg.target });
    } catch (e) {
      figma.ui.postMessage({ type: 'error', text: String(e && e.message ? e.message : e) });
    }
  } else if (msg.type === 'save-code') {
    await figma.clientStorage.setAsync('pairingCode', msg.code || '');
  } else if (msg.type === 'save-target') {
    await figma.clientStorage.setAsync('target', msg.target || 'newComp');
  } else if (msg.type === 'notify') {
    figma.notify(msg.text);
  }
};

(async function () {
  var code = await figma.clientStorage.getAsync('pairingCode');
  var target = await figma.clientStorage.getAsync('target');
  figma.ui.postMessage({ type: 'init', code: code || '', target: target || 'newComp', selection: figma.currentPage.selection.length, file: figma.root.name });
})();

figma.on('selectionchange', function () {
  figma.ui.postMessage({ type: 'selection', count: figma.currentPage.selection.length });
});
