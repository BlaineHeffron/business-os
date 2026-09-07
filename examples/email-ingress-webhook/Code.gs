/**
 * BusinessOS — Gmail → /api/webhooks/email-ingress
 * Payload matches examples/gmail-webhook-trigger (issue #12).
 * Configure via Script Properties. Do not hard-code secrets.
 */

var DEFAULT_PROCESSED_LABEL = 'bos-webhooked';
var DEFAULT_MAX_THREADS = 20;

function installTrigger() {
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
  var secret = requiredProp_(props, 'WEBHOOK_SECRET');
  var query = requiredProp_(props, 'GMAIL_QUERY');
  var ruleId = props.getProperty('RULE_ID');
  var labelName = props.getProperty('PROCESSED_LABEL') || DEFAULT_PROCESSED_LABEL;
  var maxThreads = parseInt(props.getProperty('MAX_THREADS') || String(DEFAULT_MAX_THREADS), 10);

  var label = GmailApp.getUserLabelByName(labelName) || GmailApp.createLabel(labelName);
  GmailApp.search(query, 0, maxThreads).forEach(function (thread) {
    if (threadHasLabel_(thread, labelName)) {
      return;
    }
    thread.getMessages().forEach(function (message) {
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
          snippet: String(message.getPlainBody() || '').slice(0, 280),
          date: message.getDate().toISOString()
        }
      };
      if (ruleId && String(ruleId).trim()) {
        payload.ruleId = String(ruleId).trim();
      }
      var response = UrlFetchApp.fetch(webhookUrl, {
        method: 'post',
        contentType: 'application/json',
        payload: JSON.stringify(payload),
        headers: { Authorization: 'Bearer ' + secret },
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
