// Starts and stops the node and follows the app's life: background,
// foreground and pairing links. The Android counterpart of
// apps/ios/Resonator/NodeHost.swift.

package am.resonator.client.android

import am.resonator.client.ffi.NodeConfig
import am.resonator.client.ffi.ResonatorException
import am.resonator.client.ffi.ResonatorNode
import android.content.Context
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.Executors

class NodeHost(private val context: Context, private val onState: (State) -> Unit) {

    init {
        // Loads the node library through Android first, so that it gets the
        // app's JavaVM and Context (crates/resonator-ffi/src/android.rs);
        // JNA then uses the loaded library.
        System.loadLibrary("resonator_ffi")
    }

    sealed class State {
        object Starting : State()
        data class Ready(val url: String) : State()
        data class Busy(val text: String) : State()
        data class Failed(val text: String) : State()
    }

    var state: State = State.Starting
        private set

    private var node: ResonatorNode? = null
    private var starting = false

    /** Calls run one after another, off the main thread. */
    private val queue = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())

    private fun publish(next: State) {
        state = next
        onState(next)
    }

    /** The device's name as people know it, else its model. */
    private val deviceName: String
        get() = android.provider.Settings.Global.getString(
            context.contentResolver, android.provider.Settings.Global.DEVICE_NAME
        ) ?: Build.MODEL

    fun start() {
        if (node != null || starting) return
        starting = true
        publish(State.Starting)
        val name = deviceName
        queue.execute {
            val result = runCatching {
                val dir = File(context.filesDir, "resonator").apply { mkdirs() }
                ResonatorNode.start(
                    NodeConfig(
                        dataDir = dir.path,
                        account = null,
                        deviceName = name,
                        allowRemote = false,
                        extraToml = extraToml(),
                    )
                )
            }
            main.post {
                starting = false
                result.fold(
                    onSuccess = {
                        node = it
                        showStartPage()
                    },
                    onFailure = { publish(State.Failed("The node did not start: ${describe(it)}")) },
                )
            }
        }
    }

    fun retry() {
        if (node == null) start() else showStartPage()
    }

    /** The node's sign-in URL (with this start's token) for the WebView. */
    private fun showStartPage() {
        val url = node?.signedInUrl()
        publish(if (url != null) State.Ready(url) else State.Failed("The node gave no address to open."))
    }

    /** Whether `url` is the node's own (stays in the WebView). */
    fun isNodeUrl(url: String): Boolean {
        val start = (state as? State.Ready)?.url ?: return false
        return runCatching {
            val a = URL(url)
            val b = URL(start)
            a.host == b.host && a.port == b.port
        }.getOrDefault(false)
    }

    fun background() {
        val node = node ?: return
        queue.execute { runCatching { node.enterBackground(4_000u) } }
    }

    fun foreground() {
        val current = node
        if (current == null) {
            start()
            return
        }
        if (!current.isRunning()) {
            node = null
            start()
            return
        }
        val page = (state as? State.Ready)?.url
        queue.execute {
            runCatching { current.enterForeground(10_000u) }
            // The page may have lost the node while the app was away; if it
            // does not answer, start it again (a new port, a fresh page).
            if (page != null && !answers(page)) {
                Log.i(TAG, "foreground: node does not answer /healthz; restarting it")
                current.stop()
                main.post {
                    if (node === current) {
                        node = null
                        start()
                    }
                }
            }
        }
    }

    fun stop() {
        val node = node ?: return
        this.node = null
        queue.execute { node.stop() }
    }

    /** `resonator:pair/...` pairs this device; other links are left to the client. */
    fun open(link: String) {
        if (link.startsWith("resonator:pair/")) pair(link)
    }

    private fun pair(link: String) {
        val node = node ?: return
        publish(State.Busy(context.getString(R.string.pairing)))
        val name = deviceName
        queue.execute {
            val result = runCatching { node.pairJoin(link, name, 180_000u) }
            main.post {
                result.fold(
                    // The node restarted as a device of that person, at the
                    // same address with the same token.
                    onSuccess = { showStartPage() },
                    onFailure = { publish(State.Failed("Pairing failed: ${describe(it)}")) },
                )
            }
        }
    }

    companion object {
        private const val TAG = "resonator"

        /** What went wrong, in words (the bindings' own text is `detail=...`). */
        private fun describe(error: Throwable): String = when (error) {
            is ResonatorException.Config -> "invalid configuration: ${error.detail}"
            is ResonatorException.Node -> error.detail
            is ResonatorException.Stopped -> "the node is stopped"
            else -> error.message ?: error.toString()
        }

        /**
         * Settings laid over the device defaults: the iroh relay the node
         * keeps a home on and the push relay (build settings; empty leaves
         * the defaults).
         */
        fun extraToml(): String {
            val toml = StringBuilder()
            val iroh = BuildConfig.IROH_RELAY.trim()
            if (iroh.isNotEmpty() && !iroh.contains('"')) toml.append("[federation]\nrelay = \"$iroh\"\n")
            val push = BuildConfig.PUSH_RELAY.trim()
            if (push.isNotEmpty() && !push.contains('"')) toml.append("[push]\nrelay = \"$push\"\n")
            return toml.toString()
        }

        /** Whether the node at `start` answers `/healthz` (within 3 s, twice). */
        private fun answers(start: String): Boolean {
            val base = URL(start)
            val url = URL(base.protocol, base.host, base.port, "/healthz")
            repeat(2) {
                val ok = runCatching {
                    val connection = url.openConnection() as HttpURLConnection
                    connection.connectTimeout = 3_000
                    connection.readTimeout = 3_000
                    try {
                        connection.responseCode < 500
                    } finally {
                        connection.disconnect()
                    }
                }.getOrDefault(false)
                if (ok) return true
            }
            return false
        }
    }
}
