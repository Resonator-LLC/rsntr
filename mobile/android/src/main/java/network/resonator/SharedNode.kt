// A node directory shared by every app of one vendor on the device
// (docs/shared-node.md).
//
// Android isolates apps by Linux uid, so apps can share one SQLite
// database only when they run as the same uid: same signing key and the
// same `android:sharedUserId` in every manifest, declared from each app's
// first release. One package of the family HOSTS the directory under its
// `noBackupFilesDir` (Auto Backup must not clone the device identity);
// the others open it through that package's context. Whoever is started
// serves the node (the lease); an app going to the background hands the
// lease over by suspending unless a foreground Service keeps it serving.
// Every Node call blocks: keep them on the handle's executor.
package network.resonator

import android.content.Context
import android.os.Process
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import java.io.File
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import uniffi.resonator_ffi.AppInfo
import uniffi.resonator_ffi.Node
import uniffi.resonator_ffi.ServePolicy
import uniffi.resonator_ffi.ServingInfo

object SharedNode {
    const val DIR_NAME = "rsntr"

    /** True when [hostPackage] is installed and runs as this process's uid. */
    fun isShared(context: Context, hostPackage: String): Boolean {
        if (hostPackage == context.packageName) return true
        return try {
            context.packageManager.getPackageUid(hostPackage, 0) == Process.myUid()
        } catch (e: Exception) {
            false
        }
    }

    /**
     * The shared directory: `<host>/no_backup/rsntr` when [hostPackage] runs
     * as this uid, else this app's own `no_backup/rsntr` (a private node,
     * logged as such).
     */
    fun directory(context: Context, hostPackage: String = context.packageName): File {
        val base = if (isShared(context, hostPackage)) {
            context.createPackageContext(hostPackage, 0).noBackupFilesDir
        } else {
            android.util.Log.w("SharedNode", "$hostPackage does not share this uid; using a private node")
            context.noBackupFilesDir
        }
        return File(base, DIR_NAME).apply { mkdirs() }
    }

    /**
     * Opens the shared node as [app], registers it, and wires the process
     * lifecycle: ON_START resumes (taking the lease when [policy] allows),
     * ON_STOP suspends unless [Handle.keepServingInBackground] is set by a
     * foreground Service.
     */
    fun open(
        context: Context,
        app: AppInfo,
        offline: Boolean = false,
        hostPackage: String = context.packageName,
        policy: ServePolicy = ServePolicy.SERVE_IF_FREE,
        wireLifecycle: Boolean = true,
    ): Handle {
        val dir = directory(context, hostPackage)
        val node = Node.open(dir.absolutePath, offline, app, policy)
        val handle = Handle(node, dir)
        if (wireLifecycle) {
            ProcessLifecycleOwner.get().lifecycle.addObserver(handle)
        }
        return handle
    }

    class Handle internal constructor(val node: Node, val directory: File) : DefaultLifecycleObserver {
        /** Every Node call blocks; run them here, in order. */
        val executor: ExecutorService = Executors.newSingleThreadExecutor()

        /** Set by a foreground Service (`dataSync`) that keeps the node serving while the UI is stopped. */
        @Volatile var keepServingInBackground = false

        /** Runs after every resume: entrainments end on suspend, so re-entrain here. */
        var onResume: ((Node) -> Unit)? = null
        var onSuspend: (() -> Unit)? = null

        override fun onStart(owner: LifecycleOwner) {
            executor.execute { resumeNow() }
        }

        override fun onStop(owner: LifecycleOwner) {
            if (!keepServingInBackground) executor.execute { suspendNow() }
        }

        /** Lets go of the directory (serving, entrainments, the database). */
        fun suspendNow() {
            node.suspend()
            onSuspend?.invoke()
        }

        /** Comes back; returns who serves now. */
        fun resumeNow(): ServingInfo? {
            val info = try { node.resume() } catch (e: Exception) { null }
            onResume?.invoke(node)
            return info
        }

        fun close() {
            ProcessLifecycleOwner.get().lifecycle.removeObserver(this)
            executor.execute { node.suspend() }
            executor.shutdown()
        }
    }
}
