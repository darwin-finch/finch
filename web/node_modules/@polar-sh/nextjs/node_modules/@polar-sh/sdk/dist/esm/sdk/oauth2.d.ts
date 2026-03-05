import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { IntrospectTokenRequest } from "../models/components/introspecttokenrequest.js";
import { IntrospectTokenResponse } from "../models/components/introspecttokenresponse.js";
import { RevokeTokenRequest } from "../models/components/revoketokenrequest.js";
import { RevokeTokenResponse } from "../models/components/revoketokenresponse.js";
import { TokenResponse } from "../models/components/tokenresponse.js";
import { Oauth2AuthorizeResponseOauth2Authorize } from "../models/operations/oauth2authorize.js";
import { Oauth2RequestTokenRequestBody } from "../models/operations/oauth2requesttoken.js";
import { Oauth2UserinfoResponseOauth2Userinfo } from "../models/operations/oauth2userinfo.js";
import { Clients } from "./clients.js";
export declare class Oauth2 extends ClientSDK {
    private _clients?;
    get clients(): Clients;
    /**
     * Authorize
     */
    authorize(options?: RequestOptions): Promise<Oauth2AuthorizeResponseOauth2Authorize>;
    /**
     * Request Token
     *
     * @remarks
     * Request an access token using a valid grant.
     */
    token(request: Oauth2RequestTokenRequestBody, options?: RequestOptions): Promise<TokenResponse>;
    /**
     * Revoke Token
     *
     * @remarks
     * Revoke an access token or a refresh token.
     */
    revoke(request: RevokeTokenRequest, options?: RequestOptions): Promise<RevokeTokenResponse>;
    /**
     * Introspect Token
     *
     * @remarks
     * Get information about an access token.
     */
    introspect(request: IntrospectTokenRequest, options?: RequestOptions): Promise<IntrospectTokenResponse>;
    /**
     * Get User Info
     *
     * @remarks
     * Get information about the authenticated user.
     */
    userinfo(options?: RequestOptions): Promise<Oauth2UserinfoResponseOauth2Userinfo>;
}
//# sourceMappingURL=oauth2.d.ts.map