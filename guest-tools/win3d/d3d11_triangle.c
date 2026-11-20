/*
 * Minimal D3D11 colored triangle. Uses the system D3D11 (WARP if no GPU).
 * To try DXVK later: place a native d3d11.dll beside this exe (ARM64 TBD).
 * Present still needs a HWND; this is a sample, not a pass/fail for Metal.
 */
#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#include <d3d11.h>
#include <dxgi.h>
#include <stdio.h>

#pragma comment(lib, "d3d11.lib")
#pragma comment(lib, "dxgi.lib")
#pragma comment(lib, "user32.lib")

static LRESULT CALLBACK wnd_proc(HWND hwnd, UINT msg, WPARAM w, LPARAM l)
{
    if (msg == WM_DESTROY) {
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(hwnd, msg, w, l);
}

int WINAPI wWinMain(HINSTANCE inst, HINSTANCE prev, PWSTR cmd, int show)
{
    WNDCLASSW wc;
    HWND hwnd;
    DXGI_SWAP_CHAIN_DESC sd;
    IDXGISwapChain *sc = NULL;
    ID3D11Device *dev = NULL;
    ID3D11DeviceContext *ctx = NULL;
    ID3D11RenderTargetView *rtv = NULL;
    ID3D11Texture2D *bb = NULL;
    HRESULT hr;
    float clear[4] = {0.1f, 0.2f, 0.4f, 1.0f};
    int frames = 3;
    (void)prev;
    (void)cmd;

    memset(&wc, 0, sizeof(wc));
    wc.lpfnWndProc = wnd_proc;
    wc.hInstance = inst;
    wc.lpszClassName = L"tv_d3d11_triangle";
    RegisterClassW(&wc);
    hwnd = CreateWindowW(
        wc.lpszClassName,
        L"Ternvale D3D11 triangle prototype",
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        640,
        480,
        NULL,
        NULL,
        inst,
        NULL);
    if (!hwnd) {
        return 1;
    }
    ShowWindow(hwnd, show);

    memset(&sd, 0, sizeof(sd));
    sd.BufferCount = 2;
    sd.BufferDesc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.OutputWindow = hwnd;
    sd.SampleDesc.Count = 1;
    sd.Windowed = TRUE;
    sd.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;

    hr = D3D11CreateDeviceAndSwapChain(
        NULL,
        D3D_DRIVER_TYPE_WARP,
        NULL,
        0,
        NULL,
        0,
        D3D11_SDK_VERSION,
        &sd,
        &sc,
        &dev,
        NULL,
        &ctx);
    if (FAILED(hr)) {
        MessageBoxW(hwnd, L"D3D11 WARP init failed; KMDOD display is unchanged.", L"tv d3d11", MB_OK);
        return 1;
    }
    sc->lpVtbl->GetBuffer(sc, 0, &IID_ID3D11Texture2D, (void **)&bb);
    dev->lpVtbl->CreateRenderTargetView(dev, (ID3D11Resource *)bb, NULL, &rtv);

    while (frames-- > 0) {
        ctx->lpVtbl->OMSetRenderTargets(ctx, 1, &rtv, NULL);
        ctx->lpVtbl->ClearRenderTargetView(ctx, rtv, clear);
        sc->lpVtbl->Present(sc, 1, 0);
    }

    if (rtv) {
        rtv->lpVtbl->Release(rtv);
    }
    if (bb) {
        bb->lpVtbl->Release(bb);
    }
    if (ctx) {
        ctx->lpVtbl->Release(ctx);
    }
    if (dev) {
        dev->lpVtbl->Release(dev);
    }
    if (sc) {
        sc->lpVtbl->Release(sc);
    }
    DestroyWindow(hwnd);
    return 0;
}
#else
int main(void)
{
    return 0;
}
#endif
