package com.tcc

import android.content.pm.ApplicationInfo
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.Editable
import android.text.TextWatcher
import android.util.Log
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.EditorInfo
import android.widget.EditText
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.core.widget.NestedScrollView
import java.io.BufferedReader
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStreamReader
import java.net.HttpURLConnection
import java.net.URL
import java.nio.charset.StandardCharsets
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import org.json.JSONObject

class MainActivity : AppCompatActivity() {

    private val TAG = "TCC-MainActivity"
    private val executor: ExecutorService = Executors.newSingleThreadExecutor()
    private val handler = Handler(Looper.getMainLooper())

    // UI components
    private lateinit var outputTextView: TextView
    private lateinit var inputEditText: EditText
    private lateinit var scrollView: NestedScrollView
    private lateinit var statusTextView: TextView

    // Bridge process
    private var bridgeProcess: Process? = null
    private var bridgeOutputReader: Thread? = null
    private var bridgeErrorReader: Thread? = null

    // WebSocket
    private var webSocket: WebSocket? = null
    private val okHttpClient = OkHttpClient.Builder()
        .hostnameVerifier { _, _ -> true } // Accept all hostnames for localhost
        .build()

    // Session state
    private var sessionId: String? = null
    private var isBridgeReady = false
    private var isWebSocketConnected = false

    // Constants
    private val BRIDGE_HTTP_URL = "http://localhost:8443"
    private val BRIDGE_WS_URL = "ws://localhost:8444/pty"
    private val HEALTH_CHECK_INTERVAL_MS = 500L
    private val MAX_HEALTH_CHECK_ATTEMPTS = 60 // 30 seconds max

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setupUI()
        startBridge()
    }

    private fun setupUI() {
        // Root layout: vertical LinearLayout
        val rootLayout = android.widget.LinearLayout(this).apply {
            orientation = android.widget.LinearLayout.VERTICAL
            layoutParams = android.widget.LinearLayout.LayoutParams(
                android.widget.LinearLayout.LayoutParams.MATCH_PARENT,
                android.widget.LinearLayout.LayoutParams.MATCH_PARENT
            )
            setPadding(16, 16, 16, 16)
        }

        // Status bar
        statusTextView = TextView(this).apply {
            text = "Starting bridge..."
            textSize = 14f
            setTextColor(android.graphics.Color.parseColor("#888888"))
            gravity = Gravity.CENTER_HORIZONTAL
            layoutParams = android.widget.LinearLayout.LayoutParams(
                android.widget.LinearLayout.LayoutParams.MATCH_PARENT,
                android.widget.LinearLayout.LayoutParams.WRAP_CONTENT
            )
            setPadding(0, 0, 0, 8)
        }
        rootLayout.addView(statusTextView)

        // Output area (scrollable)
        scrollView = NestedScrollView(this).apply {
            layoutParams = android.widget.LinearLayout.LayoutParams(
                android.widget.LinearLayout.LayoutParams.MATCH_PARENT,
                0,
                1f
            )
            fillViewport = true
        }

        outputTextView = TextView(this).apply {
            textSize = 14f
            typeface = android.graphics.Typeface.MONOSPACE
            setTextColor(android.graphics.Color.parseColor("#E0E0E0"))
            setBackgroundColor(android.graphics.Color.parseColor("#1E1E1E"))
            layoutParams = android.widget.LinearLayout.LayoutParams(
                android.widget.LinearLayout.LayoutParams.MATCH_PARENT,
                android.widget.LinearLayout.LayoutParams.WRAP_CONTENT
            )
            setPadding(12, 12, 12, 12)
            gravity = Gravity.TOP or Gravity.START
        }
        scrollView.addView(outputTextView)
        rootLayout.addView(scrollView)

        // Input area
        val inputLayout = android.widget.LinearLayout(this).apply {
            orientation = android.widget.LinearLayout.HORIZONTAL
            layoutParams = android.widget.LinearLayout.LayoutParams(
                android.widget.LinearLayout.LayoutParams.MATCH_PARENT,
                android.widget.LinearLayout.LayoutParams.WRAP_CONTENT
            )
            setPadding(0, 8, 0, 0)
        }

        inputEditText = EditText(this).apply {
            layoutParams = android.widget.LinearLayout.LayoutParams(
                0,
                android.widget.LinearLayout.LayoutParams.WRAP_CONTENT,
                1f
            )
            hint = "Type command..."
            textSize = 14f
            typeface = android.graphics.Typeface.MONOSPACE
            setTextColor(android.graphics.Color.parseColor("#E0E0E0"))
            setHintTextColor(android.graphics.Color.parseColor("#666666"))
            setBackgroundColor(android.graphics.Color.parseColor("#2D2D2D"))
            setPadding(12, 12, 12, 12)
            imeOptions = EditorInfo.IME_ACTION_SEND
            setOnEditorActionListener { _, actionId, _ ->
                if (actionId == EditorInfo.IME_ACTION_SEND || actionId == KeyEvent.KEYCODE_ENTER) {
                    sendInput()
                    true
                } else {
                    false
                }
            }
            addTextChangedListener(object : TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
                override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {}
                override fun afterTextChanged(s: Editable?) {}
            })
            isEnabled = false
        }
        inputLayout.addView(inputEditText)

        rootLayout.addView(inputLayout)
        setContentView(rootLayout)
    }

    private fun startBridge() {
        executor.execute {
            try {
                updateStatus("Extracting bridge binary...")
                val bridgeFile = extractBridgeBinary()

                updateStatus("Launching bridge process...")
                launchBridgeProcess(bridgeFile)

                updateStatus("Waiting for bridge health check...")
                waitForHealthCheck()

                updateStatus("Creating PTY session...")
                createPtySession()

                updateStatus("Connecting WebSocket...")
                connectWebSocket()

            } catch (e: Exception) {
                Log.e(TAG, "Bridge startup failed", e)
                runOnUiThread {
                    appendOutput("\n[ERROR] ${e.message}\n")
                    updateStatus("Failed: ${e.message}")
                }
            }
        }
    }

    private fun extractBridgeBinary(): File {
        val destFile = File(filesDir, "tcc-bridge")

        // If already extracted and executable, reuse it
        if (destFile.exists() && destFile.canExecute()) {
            Log.d(TAG, "Bridge binary already extracted at ${destFile.absolutePath}")
            return destFile
        }

        // The native library is extracted to the app's native library directory
        val nativeLibDir = applicationInfo.nativeLibraryDir
        val sourceFile = File(nativeLibDir, "libtcc-bridge.so")

        if (!sourceFile.exists()) {
            throw IllegalStateException("Bridge binary not found at $sourceFile")
        }

        val inputStream = java.io.FileInputStream(sourceFile)
        val outputStream = FileOutputStream(destFile)
        try {
            inputStream.copyTo(outputStream)
        } finally {
            inputStream.close()
            outputStream.close()
        }

        // Make executable
        if (!destFile.setExecutable(true, false)) {
            throw IOException("Failed to make bridge binary executable")
        }

        Log.d(TAG, "Bridge binary extracted to ${destFile.absolutePath} (${destFile.length()} bytes)")
        return destFile
    }

    private fun launchBridgeProcess(bridgeFile: File) {
        val processBuilder = ProcessBuilder(bridgeFile.absolutePath)
            .directory(filesDir)
            .redirectErrorStream(false)

        bridgeProcess = processBuilder.start()

        // Read stdout
        bridgeOutputReader = Thread {
            val reader = BufferedReader(InputStreamReader(bridgeProcess!!.inputStream))
            var line: String?
            while (reader.readLine().also { line = it } != null) {
                Log.d(TAG, "BRIDGE STDOUT: $line")
            }
        }.apply { start() }

        // Read stderr
        bridgeErrorReader = Thread {
            val reader = BufferedReader(InputStreamReader(bridgeProcess!!.errorStream))
            var line: String?
            while (reader.readLine().also { line = it } != null) {
                Log.e(TAG, "BRIDGE STDERR: $line")
            }
        }.apply { start() }

        // Give process time to start
        Thread.sleep(1000)

        if (bridgeProcess!!.isAlive) {
            Log.d(TAG, "Bridge process started (PID: ${getPid()})")
        } else {
            val exitCode = bridgeProcess!!.exitValue()
            throw IOException("Bridge process exited immediately with code $exitCode")
        }
    }

    private fun getPid(): Int {
        try {
            val pidField = Process::class.java.getDeclaredField("pid")
            pidField.isAccessible = true
            return pidField.getInt(bridgeProcess)
        } catch (e: Exception) {
            return -1
        }
    }

    private fun waitForHealthCheck() {
        var attempts = 0
        while (attempts < MAX_HEALTH_CHECK_ATTEMPTS) {
            if (!bridgeProcess!!.isAlive) {
                throw IOException("Bridge process died during health check")
            }

            try {
                val url = URL("$BRIDGE_HTTP_URL/health")
                val connection = url.openConnection() as HttpURLConnection
                connection.requestMethod = "GET"
                connection.connectTimeout = 2000
                connection.readTimeout = 2000

                if (connection.responseCode == 200) {
                    val response = connection.inputStream.bufferedReader().use { it.readText() }
                    Log.d(TAG, "Health check OK: $response")
                    isBridgeReady = true
                    return
                }
            } catch (e: Exception) {
                // Expected during startup, retry
                Log.d(TAG, "Health check attempt ${attempts + 1} failed: ${e.message}")
            }

            attempts++
            Thread.sleep(HEALTH_CHECK_INTERVAL_MS)
        }

        throw IOException("Health check timeout after ${MAX_HEALTH_CHECK_ATTEMPTS} attempts")
    }

    private fun createPtySession() {
        val url = URL("$BRIDGE_HTTP_URL/rpc")
        val connection = url.openConnection() as HttpURLConnection
        connection.requestMethod = "POST"
        connection.setRequestProperty("Content-Type", "application/json")
        connection.doOutput = true
        connection.connectTimeout = 5000
        connection.readTimeout = 5000

        val requestJson = JSONObject().apply {
            put("jsonrpc", "2.0")
            put("id", 1)
            put("method", "bridge.create_session")
            put("params", JSONObject().apply {
                put("rows", 40)
                put("cols", 120)
            })
        }.toString()

        connection.outputStream.use { output ->
            output.write(requestJson.toByteArray(StandardCharsets.UTF_8))
        }

        if (connection.responseCode != 200) {
            val error = connection.errorStream?.bufferedReader()?.use { it.readText() } ?: "Unknown error"
            throw IOException("create_session failed: HTTP ${connection.responseCode} - $error")
        }

        val response = connection.inputStream.bufferedReader().use { it.readText() }
        val responseJson = JSONObject(response)

        if (responseJson.has("error")) {
            throw IOException("create_session error: ${responseJson.getJSONObject("error")}")
        }

        val result = responseJson.getJSONObject("result")
        sessionId = result.getString("session_id")
        Log.d(TAG, "PTY session created: $sessionId")
    }

    private fun connectWebSocket() {
        val request = Request.Builder()
            .url(BRIDGE_WS_URL)
            .build()

        webSocket = okHttpClient.newWebSocket(request, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: okhttp3.Response) {
                Log.d(TAG, "WebSocket connected")
                isWebSocketConnected = true
                runOnUiThread {
                    updateStatus("Connected - Ready")
                    inputEditText.isEnabled = true
                    appendOutput("\r\n\$ ")
                }
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                // Handle JSON-RPC responses or PTY output
                try {
                    val json = JSONObject(text)
                    if (json.has("method") && json.getString("method") == "pty.output") {
                        val data = json.getJSONObject("params").getString("data")
                        runOnUiThread { appendOutput(data) }
                    } else if (json.has("result")) {
                        Log.d(TAG, "WebSocket JSON-RPC result: ${json.get("result")}")
                    } else if (json.has("error")) {
                        Log.e(TAG, "WebSocket JSON-RPC error: ${json.get("error")}")
                    }
                } catch (e: Exception) {
                    // Not JSON, treat as raw PTY output
                    runOnUiThread { appendOutput(text) }
                }
            }

            override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
                // Binary PTY output
                val text = bytes.decodeString(StandardCharsets.UTF_8)
                runOnUiThread { appendOutput(text) }
            }

            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                Log.d(TAG, "WebSocket closing: $code - $reason")
                webSocket.close(1000, "Client closing")
                isWebSocketConnected = false
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                Log.d(TAG, "WebSocket closed: $code - $reason")
                isWebSocketConnected = false
                runOnUiThread {
                    updateStatus("Disconnected")
                    inputEditText.isEnabled = false
                }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: okhttp3.Response?) {
                Log.e(TAG, "WebSocket failure", t)
                isWebSocketConnected = false
                runOnUiThread {
                    updateStatus("Connection failed: ${t.message}")
                    inputEditText.isEnabled = false
                }
            }
        })
    }

    private fun sendInput() {
        val text = inputEditText.text.toString()
        if (text.isEmpty()) return

        inputEditText.setText("")

        if (!isWebSocketConnected || sessionId == null) {
            appendOutput("\n[Not connected]\n\$ ")
            return
        }

        // Echo locally
        appendOutput(text + "\n")

        // Send via WebSocket as JSON-RPC pty.write
        val requestJson = JSONObject().apply {
            put("jsonrpc", "2.0")
            put("id", System.currentTimeMillis())
            put("method", "pty.write")
            put("params", JSONObject().apply {
                put("session_id", sessionId)
                put("data", text + "\n")
            })
        }.toString()

        webSocket?.send(requestJson)
    }

    private fun appendOutput(text: String) {
        outputTextView.append(text)
        // Auto-scroll to bottom
        scrollView.post {
            scrollView.fullScroll(View.FOCUS_DOWN)
        }
    }

    private fun updateStatus(message: String) {
        runOnUiThread {
            statusTextView.text = message
        }
    }

    override fun onDestroy() {
        super.onDestroy()

        // Close WebSocket
        webSocket?.close(1000, "Activity destroyed")

        // Terminate bridge process
        bridgeProcess?.destroy()
        try {
            bridgeProcess?.waitFor()
        } catch (e: InterruptedException) {
            Thread.currentThread().interrupt()
        }

        // Stop reader threads
        bridgeOutputReader?.interrupt()
        bridgeErrorReader?.interrupt()

        executor.shutdown()
    }

    companion object {
        // Trust all certificates for localhost development
        @Suppress("UNUSED_PARAMETER")
        private val trustAllCerts: Array<TrustManager> = arrayOf(
            object : X509TrustManager {
                override fun checkClientTrusted(chain: Array<java.security.cert.X509Certificate>?, authType: String?) {}
                override fun checkServerTrusted(chain: Array<java.security.cert.X509Certificate>?, authType: String?) {}
                override fun getAcceptedIssuers(): Array<java.security.cert.X509Certificate?> = emptyArray()
            }
        )
    }
}