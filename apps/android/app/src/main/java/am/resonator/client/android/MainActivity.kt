// The client in a WebView: the node's pages stay here, other links open in
// the browser, file inputs open the system picker.

package am.resonator.client.android

import android.annotation.SuppressLint
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.TextView

class MainActivity : Activity() {

    private lateinit var host: NodeHost
    private lateinit var root: FrameLayout
    private lateinit var webView: WebView
    private lateinit var overlay: LinearLayout
    private lateinit var message: TextView
    private lateinit var spinner: ProgressBar
    private lateinit var retry: Button
    private var loadedUrl: String? = null
    private var fileCallback: ValueCallback<Array<Uri>>? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        buildViews()
        host = NodeHost(applicationContext) { render(it) }
        host.start()
        intent?.data?.let { host.open(it.toString()) }
    }

    @SuppressLint("SetJavaScriptEnabled")
    private fun buildViews() {
        root = FrameLayout(this)
        // Keep the page out from under the status and navigation bars.
        root.setOnApplyWindowInsetsListener { view, insets ->
            @Suppress("DEPRECATION")
            view.setPadding(
                insets.systemWindowInsetLeft, insets.systemWindowInsetTop,
                insets.systemWindowInsetRight, insets.systemWindowInsetBottom,
            )
            insets
        }
        webView = WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            settings.mediaPlaybackRequiresUserGesture = true
            settings.allowFileAccess = false
            settings.allowContentAccess = true
            // The page scales its whole layout with the system font size
            // (--system-text-scale, below), not only the text.
            settings.textZoom = 100
            webViewClient = Client()
            webChromeClient = Chrome()
            visibility = View.INVISIBLE
        }
        root.addView(webView, FrameLayout.LayoutParams(MATCH, MATCH))

        spinner = ProgressBar(this)
        message = TextView(this).apply {
            gravity = Gravity.CENTER
            setPadding(0, 32, 0, 32)
        }
        retry = Button(this).apply {
            text = getString(R.string.try_again)
            setOnClickListener { host.retry() }
        }
        overlay = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER
            setPadding(64, 64, 64, 64)
            addView(spinner)
            addView(message)
            addView(retry)
        }
        root.addView(overlay, FrameLayout.LayoutParams(MATCH, MATCH))
        setContentView(root)
    }

    private fun render(state: NodeHost.State) {
        when (state) {
            is NodeHost.State.Ready -> {
                overlay.visibility = View.GONE
                webView.visibility = View.VISIBLE
                if (state.url != loadedUrl) {
                    loadedUrl = state.url
                    webView.loadUrl(state.url)
                }
            }
            is NodeHost.State.Starting -> waiting(getString(R.string.starting))
            is NodeHost.State.Busy -> waiting(state.text)
            is NodeHost.State.Failed -> {
                overlay.visibility = View.VISIBLE
                webView.visibility = View.INVISIBLE
                spinner.visibility = View.GONE
                message.text = state.text
                retry.visibility = View.VISIBLE
                loadedUrl = null
            }
        }
    }

    private fun waiting(text: String) {
        overlay.visibility = View.VISIBLE
        webView.visibility = View.INVISIBLE
        spinner.visibility = View.VISIBLE
        message.text = text
        retry.visibility = View.GONE
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        intent.data?.let { host.open(it.toString()) }
    }

    override fun onStart() {
        super.onStart()
        host.foreground()
    }

    override fun onStop() {
        super.onStop()
        host.background()
    }

    override fun onDestroy() {
        if (isFinishing) host.stop()
        webView.destroy()
        super.onDestroy()
    }

    @Deprecated("Activity's own back handling; fine on every supported version")
    override fun onBackPressed() {
        if (webView.visibility == View.VISIBLE && webView.canGoBack()) {
            webView.goBack()
        } else {
            @Suppress("DEPRECATION")
            super.onBackPressed()
        }
    }

    @Deprecated("Platform Activity result API")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (requestCode == PICK_FILES) {
            fileCallback?.onReceiveValue(WebChromeClient.FileChooserParams.parseResult(resultCode, data))
            fileCallback = null
            return
        }
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
    }

    /** The system font size, for the page's "As the system" text size. */
    private fun applyTextScale(view: WebView) {
        val scale = resources.configuration.fontScale.coerceIn(0.5f, 3f)
        view.evaluateJavascript(
            "document.documentElement.style.setProperty('--system-text-scale','$scale')", null,
        )
    }

    private inner class Client : WebViewClient() {
        override fun onPageCommitVisible(view: WebView, url: String) = applyTextScale(view)

        override fun onPageFinished(view: WebView, url: String) = applyTextScale(view)

        override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
            val url = request.url.toString()
            if (host.isNodeUrl(url)) return false
            if (url.startsWith("resonator:")) {
                host.open(url)
                return true
            }
            try {
                startActivity(Intent(Intent.ACTION_VIEW, request.url))
            } catch (_: ActivityNotFoundException) {
            }
            return true
        }

        override fun onRenderProcessGone(view: WebView, detail: android.webkit.RenderProcessGoneDetail): Boolean {
            // The page's process died; load it again in a fresh WebView.
            root.removeView(webView)
            webView.destroy()
            loadedUrl = null
            root.removeAllViews()
            buildViews()
            render(host.state)
            return true
        }
    }

    private inner class Chrome : WebChromeClient() {
        override fun onShowFileChooser(
            view: WebView,
            callback: ValueCallback<Array<Uri>>,
            params: FileChooserParams,
        ): Boolean {
            fileCallback?.onReceiveValue(null)
            fileCallback = callback
            return try {
                @Suppress("DEPRECATION")
                startActivityForResult(params.createIntent(), PICK_FILES)
                true
            } catch (_: ActivityNotFoundException) {
                fileCallback = null
                false
            }
        }
    }

    companion object {
        private const val MATCH = ViewGroup.LayoutParams.MATCH_PARENT
        private const val PICK_FILES = 1
    }
}
