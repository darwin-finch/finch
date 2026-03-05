import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { CustomerSubscription } from "../models/components/customersubscription.js";
import { CustomerPortalSubscriptionsCancelRequest, CustomerPortalSubscriptionsCancelSecurity } from "../models/operations/customerportalsubscriptionscancel.js";
import { CustomerPortalSubscriptionsGetRequest, CustomerPortalSubscriptionsGetSecurity } from "../models/operations/customerportalsubscriptionsget.js";
import { CustomerPortalSubscriptionsListRequest, CustomerPortalSubscriptionsListResponse, CustomerPortalSubscriptionsListSecurity } from "../models/operations/customerportalsubscriptionslist.js";
import { CustomerPortalSubscriptionsUpdateRequest, CustomerPortalSubscriptionsUpdateSecurity } from "../models/operations/customerportalsubscriptionsupdate.js";
import { PageIterator } from "../types/operations.js";
export declare class PolarSubscriptions extends ClientSDK {
    /**
     * List Subscriptions
     *
     * @remarks
     * List subscriptions of the authenticated customer.
     *
     * **Scopes**: `customer_portal:read` `customer_portal:write`
     */
    list(security: CustomerPortalSubscriptionsListSecurity, request: CustomerPortalSubscriptionsListRequest, options?: RequestOptions): Promise<PageIterator<CustomerPortalSubscriptionsListResponse, {
        page: number;
    }>>;
    /**
     * Get Subscription
     *
     * @remarks
     * Get a subscription for the authenticated customer.
     *
     * **Scopes**: `customer_portal:read` `customer_portal:write`
     */
    get(security: CustomerPortalSubscriptionsGetSecurity, request: CustomerPortalSubscriptionsGetRequest, options?: RequestOptions): Promise<CustomerSubscription>;
    /**
     * Update Subscription
     *
     * @remarks
     * Update a subscription of the authenticated customer.
     *
     * **Scopes**: `customer_portal:write`
     */
    update(security: CustomerPortalSubscriptionsUpdateSecurity, request: CustomerPortalSubscriptionsUpdateRequest, options?: RequestOptions): Promise<CustomerSubscription>;
    /**
     * Cancel Subscription
     *
     * @remarks
     * Cancel a subscription of the authenticated customer.
     *
     * **Scopes**: `customer_portal:write`
     */
    cancel(security: CustomerPortalSubscriptionsCancelSecurity, request: CustomerPortalSubscriptionsCancelRequest, options?: RequestOptions): Promise<CustomerSubscription>;
}
//# sourceMappingURL=polarsubscriptions.d.ts.map