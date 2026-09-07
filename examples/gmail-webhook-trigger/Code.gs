/**
 * BusinessOS — Gmail → outbound webhook trigger
 * Configure via Script Properties (see README.md). Do not hard-code secrets.
 */

var DEFAULT_PROCESSED_LABEL = 'bos-webhooked';
var DEFAULT_MAX_THREADS = 20;

function installTrigger() {
  // Remove prior clock triggers for this function to avoid duplicates.
  ScriptApp.getProjectTriggers().forEach(function (t) {
    if (t.getHandlerFunction() === 'pollAndPost') {
      ScriptApp.deleteTrigger(t);
    }
  });
  ScriptApp.newTrigger('pollAndPost').timeBased().everyMinutes(5).create();
}

function pollAndPost() {
  var props = PropertiesService.getScriptProperties();
  var webhookUrl = requiredProp_(props, 'WEBHOOK_URL');
  var query = requiredProp_(props, 'GMAIL_QUERY');
  var secret = props.getProperty('WEBHOOK_SECRET');
  var authHeader = props.getProperty('WEBHOOK_AUTH_HEADER') || 'Authorization';
  var labelName = props.getProperty('PROCESSED_LABEL') || DEFAULT_PROCESSED_LABEL;
  var maxThreads = parseInt(props.getProperty('MAX_THREADS') || String(DEFAULT_MAX_THREADS), 10);

  var label = GmailApp.getUserLabelByName(labelName);
  if (!label) {
    label = GmailApp.createLabel(labelName);
  }

  var threads = GmailApp.search(query, 0, maxThreads);
  threads.forEach(function (thread) {
    var messages = thread.getMessages();
    messages.forEach(function (message) {
      if (threadHasLabel_(thread, labelName)) {
        return;
      }
      var payload = {
        source: 'business-os.gmail-webhook-trigger',
        version: 1,
        receivedAt: new Date().toISOString(),
        query: query,
        message: {
          messageId: message.getId(),
          threadId: thread.getId(),
          from: message.getFrom(),
          to: message.getTo(),
          subject: message.getSubject(),
          snippet: message.getPlainBody().slice(0, 280),
          date: message.getDate().toISOString()
        }
      };
      var headers = {
        'Content-Type': 'application/json'
      };
      if (secret) {
        headers[authHeader] = 'Bearer ' + secret;
      }
      var response = UrlFetchApp.fetch(webhookUrl, {
        method: 'post',
        contentType: 'application/json',
        payload: JSON.stringify(payload),
        headers: headers,
        muteHttpExceptions: true,
        followRedirects: true
      });
      var code = response.getResponseCode();
      if (code >= 200 && code < 300) {
        thread.addLabel(label);
      } else {
        console.error('webhook failed', code, response.getContentText());
      }
    });
  });
}

function requiredProp_(props, key) {
  var value = props.getProperty(key);
  if (!value || !String(value).trim()) {
    throw new Error('Missing Script Property: ' + key);
  }
  return String(value).trim();
}

function threadHasLabel_(thread, labelName) {
  return thread.getLabels().some(function (l) {
    return l.getName() === labelName;
  });
}

/** Manual dry-run: logs matching threads without POSTing. */
function debugListMatches() {
  var props = PropertiesService.getScriptProperties();
  var query = requiredProp_(props, 'GMAIL_QUERY');
  var threads = GmailApp.search(query, 0, 10);
  threads.forEach(function (t) {
    console.log(t.getFirstMessageSubject(), t.getId());
  });
}
