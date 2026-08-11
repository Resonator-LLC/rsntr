// Minimal Activity exercising the uniffi bindings: node in the app's
// files dir, one local SQL write, one entrained Sympathetic point whose
// vibrations append to the screen. Every Node call blocks: keep them
// off the main thread.
package network.resonator.demo

import android.app.Activity
import android.os.Bundle
import android.widget.ScrollView
import android.widget.TextView
import java.io.File
import java.util.concurrent.Executors
import uniffi.resonator_ffi.FfiValue
import uniffi.resonator_ffi.Node
import uniffi.resonator_ffi.VibrationListener

class MainActivity : Activity(), VibrationListener {
    private val worker = Executors.newSingleThreadExecutor()
    private lateinit var log: TextView
    private var node: Node? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        log = TextView(this)
        setContentView(ScrollView(this).apply { addView(log) })

        worker.execute {
            try {
                val dir = File(filesDir, "resonator-node").absolutePath
                val n = Node(dir, false)
                node = n
                append("endpoint: ${n.endpointId()}")
                n.localExecute(
                    "CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT)",
                    listOf(),
                )
                n.localExecute(
                    "INSERT OR IGNORE INTO _projection (point_iri, kind, label, resource) " +
                        "VALUES ('urn:demo:notes-changed', 'sympathetic', 'notes changed', 'notes')",
                    listOf(),
                )
                n.entrain("urn:demo:notes-changed", this)
                append("entrained urn:demo:notes-changed; writing a note...")
                n.localExecute(
                    "INSERT INTO notes (body) VALUES (?1)",
                    listOf(FfiValue.Text("hello from android")),
                )
            } catch (e: Exception) {
                append("failed: $e")
            }
        }
    }

    override fun onVibration(point: String, seq: Long, at: String?) {
        append("vibration #$seq of $point at ${at ?: "?"}")
    }

    override fun onEnd(reason: String?) {
        append("entrainment ended: ${reason ?: "clean"}")
    }

    private fun append(line: String) {
        runOnUiThread { log.append(line + "\n") }
    }

    override fun onDestroy() {
        super.onDestroy()
        worker.execute { node?.stop() }
        worker.shutdown()
    }
}
