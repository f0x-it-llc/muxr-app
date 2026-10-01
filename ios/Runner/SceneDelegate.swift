import UIKit
import FrustEmbedding

// Tablet plan D11/R4: the floor for a usable terminal; iPadOS enforces it on
// the resize handle.
private let minimumSceneSize = CGSize(width: 480, height: 480)

class SceneDelegate: FrustSceneDelegate {
    override func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        super.scene(scene, willConnectTo: session, options: connectionOptions)
        // A resizable iPad window never shrinks below `minimumSceneSize`.
        if let ws = scene as? UIWindowScene {
            ws.sizeRestrictions?.minimumSize = minimumSceneSize
        }
    }
}
