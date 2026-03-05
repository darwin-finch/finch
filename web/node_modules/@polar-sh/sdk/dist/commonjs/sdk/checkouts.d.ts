import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { Checkout } from "../models/components/checkout.js";
import { CheckoutCreate } from "../models/components/checkoutcreate.js";
import { CheckoutPublic } from "../models/components/checkoutpublic.js";
import { CheckoutPublicConfirmed } from "../models/components/checkoutpublicconfirmed.js";
import { CheckoutsClientConfirmRequest } from "../models/operations/checkoutsclientconfirm.js";
import { CheckoutsClientGetRequest } from "../models/operations/checkoutsclientget.js";
import { CheckoutsClientUpdateRequest } from "../models/operations/checkoutsclientupdate.js";
import { CheckoutsGetRequest } from "../models/operations/checkoutsget.js";
import { CheckoutsListRequest, CheckoutsListResponse } from "../models/operations/checkoutslist.js";
import { CheckoutsUpdateRequest } from "../models/operations/checkoutsupdate.js";
import { PageIterator } from "../types/operations.js";
export declare class Checkouts extends ClientSDK {
    /**
     * List Checkout Sessions
     *
     * @remarks
     * List checkout sessions.
     *
     * **Scopes**: `checkouts:read` `checkouts:write`
     */
    list(request: CheckoutsListRequest, options?: RequestOptions): Promise<PageIterator<CheckoutsListResponse, {
        page: number;
    }>>;
    /**
     * Create Checkout Session
     *
     * @remarks
     * Create a checkout session.
     *
     * **Scopes**: `checkouts:write`
     */
    create(request: CheckoutCreate, options?: RequestOptions): Promise<Checkout>;
    /**
     * Get Checkout Session
     *
     * @remarks
     * Get a checkout session by ID.
     *
     * **Scopes**: `checkouts:read` `checkouts:write`
     */
    get(request: CheckoutsGetRequest, options?: RequestOptions): Promise<Checkout>;
    /**
     * Update Checkout Session
     *
     * @remarks
     * Update a checkout session.
     *
     * **Scopes**: `checkouts:write`
     */
    update(request: CheckoutsUpdateRequest, options?: RequestOptions): Promise<Checkout>;
    /**
     * Get Checkout Session from Client
     *
     * @remarks
     * Get a checkout session by client secret.
     */
    clientGet(request: CheckoutsClientGetRequest, options?: RequestOptions): Promise<CheckoutPublic>;
    /**
     * Update Checkout Session from Client
     *
     * @remarks
     * Update a checkout session by client secret.
     */
    clientUpdate(request: CheckoutsClientUpdateRequest, options?: RequestOptions): Promise<CheckoutPublic>;
    /**
     * Confirm Checkout Session from Client
     *
     * @remarks
     * Confirm a checkout session by client secret.
     *
     * Orders and subscriptions will be processed.
     */
    clientConfirm(request: CheckoutsClientConfirmRequest, options?: RequestOptions): Promise<CheckoutPublicConfirmed>;
}
//# sourceMappingURL=checkouts.d.ts.map