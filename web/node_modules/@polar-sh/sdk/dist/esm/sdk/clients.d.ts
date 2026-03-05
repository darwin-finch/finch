import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { OAuth2ClientConfiguration } from "../models/components/oauth2clientconfiguration.js";
import { Oauth2ClientsListRequest, Oauth2ClientsListResponse } from "../models/operations/oauth2clientslist.js";
import { Oauth2ClientsOauth2DeleteClientRequest } from "../models/operations/oauth2clientsoauth2deleteclient.js";
import { Oauth2ClientsOauth2GetClientRequest } from "../models/operations/oauth2clientsoauth2getclient.js";
import { Oauth2ClientsOauth2UpdateClientRequest } from "../models/operations/oauth2clientsoauth2updateclient.js";
import { PageIterator } from "../types/operations.js";
export declare class Clients extends ClientSDK {
    /**
     * List Clients
     *
     * @remarks
     * List OAuth2 clients.
     */
    list(request: Oauth2ClientsListRequest, options?: RequestOptions): Promise<PageIterator<Oauth2ClientsListResponse, {
        page: number;
    }>>;
    /**
     * Create Client
     *
     * @remarks
     * Create an OAuth2 client.
     */
    create(request: OAuth2ClientConfiguration, options?: RequestOptions): Promise<any>;
    /**
     * Get Client
     *
     * @remarks
     * Get an OAuth2 client by Client ID.
     */
    get(request: Oauth2ClientsOauth2GetClientRequest, options?: RequestOptions): Promise<any>;
    /**
     * Update Client
     *
     * @remarks
     * Update an OAuth2 client.
     */
    update(request: Oauth2ClientsOauth2UpdateClientRequest, options?: RequestOptions): Promise<any>;
    /**
     * Delete Client
     *
     * @remarks
     * Delete an OAuth2 client.
     */
    delete(request: Oauth2ClientsOauth2DeleteClientRequest, options?: RequestOptions): Promise<any>;
}
//# sourceMappingURL=clients.d.ts.map