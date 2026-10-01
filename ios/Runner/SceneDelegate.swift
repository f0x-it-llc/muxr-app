import UIKit
import FrustEmbedding

class SceneDelegate: FrustSceneDelegate {
    override func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        super.scene(scene, willConnectTo: session, options: connectionOptions)
        // A resizable iPad window never shrinks below a usable terminal.
        if let ws = scene as? UIWindowScene {
            ws.sizeRestrictions?.minimumSize = CGSize(width: 480, height: 480)
        }
    }
}
