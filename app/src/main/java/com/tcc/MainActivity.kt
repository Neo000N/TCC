package com.tcc

import android.os.Bundle
import androidx.appcompat.app.AppCompatActivity
import android.widget.TextView
import com.tcc.BuildConfig

class MainActivity : AppCompatActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val textView = TextView(this)
        textView.text = "TCC v0.1 — PoC H7 OK\nBuild: ${BuildConfig.VERSION_NAME}"
        textView.textSize = 24f
        textView.gravity = android.view.Gravity.CENTER
        setContentView(textView)
    }
}