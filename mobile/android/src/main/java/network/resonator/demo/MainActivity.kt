// Minimal Activity exercising the uniffi bindings on the device's shared
// node (docs/shared-node.md): every app of this vendor with the same
// signing key and `android:sharedUserId` opens the same directory under
// the host package's no_backup files; whoever is started serves it.
// One local SQL write, one declared table, one entrained Sympathetic
// point whose vibrations append to the screen (writes by the other apps
// vibrate too). Every Node call blocks: keep them on the handle's
// executor.
package network.resonator.demo

import android.app.Activity
import android.os.Bundle
import android.widget.ScrollView
import android.widget.TextView
import network.resonator.SharedNode
import uniffi.resonator_ffi.AppInfo
import uniffi.resonator_ffi.FfiValue
import uniffi.resonator_ffi.TableDecl
import uniffi.resonator_ffi.VibrationListener

class MainActivity : Activity(), VibrationListener {
    private lateinit var log: TextView
    private var shared: SharedNode.Handle? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        log = TextView(this)
        setContentView(ScrollView(this).apply { addView(log) })

        // The host package of the family (this demo hosts its own).
        val handle = SharedNode.open(this, AppInfo("network.resonator.demo", "Resonator Demo", "0.1"))
        shared = handle
        handle.onResume = { n ->
            // Entrainments end on suspend; this runs on the executor.
            runCatching { n.entrain("urn:demo:notes-changed", this) }
        }
        handle.executor.execute {
            try {
                val n = handle.node
                append("endpoint: ${n.endpointId()}; dir: ${handle.directory}")
                n.localExecute(
                    "CREATE TABLE IF NOT EXISTS demo_notes (id INTEGER PRIMARY KEY, body TEXT)",
                    listOf(),
                )
                n.declareTables(listOf(TableDecl("demo_notes", "notes typed in the demo app")))
                n.localExecute(
                    "INSERT OR IGNORE INTO _projection (point_iri, kind, label, resource) " +
                        "VALUES ('urn:demo:notes-changed', 'sympathetic', 'notes changed', 'demo_notes')",
                    listOf(),
                )
                n.entrain("urn:demo:notes-changed", this)
                append("entrained urn:demo:notes-changed; serving: ${n.servingInfo()?.appId ?: "nobody"}")
                n.localExecute(
                    "INSERT INTO demo_notes (body) VALUES (?1)",
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
        shared?.close()
    }
}
