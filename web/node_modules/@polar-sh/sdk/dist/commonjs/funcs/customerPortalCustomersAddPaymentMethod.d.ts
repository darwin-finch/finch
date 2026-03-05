import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CustomerPaymentMethodCreate } from "../models/components/customerpaymentmethodcreate.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod, CustomerPortalCustomersAddPaymentMethodSecurity } from "../models/operations/customerportalcustomersaddpaymentmethod.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Add Customer Payment Method
 *
 * @remarks
 * Add a payment method to the authenticated customer.
 *
 * **Scopes**: `customer_portal:read` `customer_portal:write`
 */
export declare function customerPortalCustomersAddPaymentMethod(client: PolarCore, security: CustomerPortalCustomersAddPaymentMethodSecurity, request: CustomerPaymentMethodCreate, options?: RequestOptions): APIPromise<Result<CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerPortalCustomersAddPaymentMethod.d.ts.map